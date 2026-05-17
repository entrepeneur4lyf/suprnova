//! SeaORM-backed queue driver. Portable across SQLite / MySQL / Postgres.
//!
//! On SQLite, uses transaction-level locking via `BEGIN` to serialize pop
//! attempts. On MySQL / Postgres, uses `SELECT ... FOR UPDATE SKIP LOCKED`.

use crate::error::FrameworkError;
use crate::queue::driver::{QueueDriver, Reservation, ReservationToken};
use crate::queue::envelope::Envelope;
use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement, TransactionTrait};
use std::time::Duration;
use uuid::Uuid;

pub struct DatabaseQueueDriver {
    db: DatabaseConnection,
    table: String,
}

impl DatabaseQueueDriver {
    pub fn new(db: DatabaseConnection, table: String) -> Self {
        Self { db, table }
    }

    fn backend(&self) -> DatabaseBackend {
        self.db.get_database_backend()
    }
}

#[async_trait]
impl QueueDriver for DatabaseQueueDriver {
    async fn push(&self, env: Envelope) -> Result<(), FrameworkError> {
        let envelope_json = env
            .to_json()
            .map_err(|e| FrameworkError::internal(format!("envelope encode: {e}")))?;
        let stmt = Statement::from_sql_and_values(
            self.backend(),
            format!(
                "INSERT INTO {} (id, job_name, envelope_json, available_at, attempts, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                self.table
            ),
            vec![
                sea_orm::Value::from(env.id.to_string()),
                sea_orm::Value::from(env.job_name.clone()),
                sea_orm::Value::from(envelope_json),
                sea_orm::Value::from(env.available_at.timestamp()),
                sea_orm::Value::from(env.attempts as i64),
                sea_orm::Value::from(env.dispatched_at.timestamp()),
            ],
        );
        self.db
            .execute(stmt)
            .await
            .map_err(|e| FrameworkError::internal(format!("queue push: {e}")))?;
        Ok(())
    }

    async fn pop(&self, visibility_timeout: Duration) -> Result<Option<Reservation>, FrameworkError> {
        let now = Utc::now().timestamp();
        let token = Uuid::new_v4().to_string();
        let reserved_until = now + visibility_timeout.as_secs() as i64;

        let lock_clause = match self.backend() {
            DatabaseBackend::Postgres | DatabaseBackend::MySql => "FOR UPDATE SKIP LOCKED",
            DatabaseBackend::Sqlite => "",
        };

        let txn = self
            .db
            .begin()
            .await
            .map_err(|e| FrameworkError::internal(format!("queue txn: {e}")))?;

        let select_sql = format!(
            "SELECT id, envelope_json FROM {} \
             WHERE available_at <= ? \
               AND (reserved_until IS NULL OR reserved_until <= ?) \
             ORDER BY available_at ASC \
             LIMIT 1 {}",
            self.table, lock_clause
        );
        let row = txn
            .query_one(Statement::from_sql_and_values(
                self.backend(),
                &select_sql,
                vec![sea_orm::Value::from(now), sea_orm::Value::from(now)],
            ))
            .await
            .map_err(|e| FrameworkError::internal(format!("queue select: {e}")))?;

        let Some(row) = row else {
            txn.commit().await.ok();
            return Ok(None);
        };

        // Use index-based access — raw SQL column names may not be introspectable.
        let id: String = row
            .try_get_by_index::<String>(0)
            .map_err(|e| FrameworkError::internal(format!("queue id col: {e}")))?;
        let envelope_json: String = row
            .try_get_by_index::<String>(1)
            .map_err(|e| FrameworkError::internal(format!("queue envelope col: {e}")))?;

        txn.execute(Statement::from_sql_and_values(
            self.backend(),
            format!(
                "UPDATE {} SET reserved_until = ?, reserved_token = ? WHERE id = ?",
                self.table
            ),
            vec![
                sea_orm::Value::from(reserved_until),
                sea_orm::Value::from(token.clone()),
                sea_orm::Value::from(id),
            ],
        ))
        .await
        .map_err(|e| FrameworkError::internal(format!("queue reserve: {e}")))?;

        txn.commit()
            .await
            .map_err(|e| FrameworkError::internal(format!("queue txn commit: {e}")))?;

        let env = Envelope::from_json(&envelope_json)
            .map_err(|e| FrameworkError::internal(format!("envelope decode: {e}")))?;

        let reservation_token = ReservationToken(
            Uuid::parse_str(&token)
                .map_err(|e| FrameworkError::internal(format!("uuid parse: {e}")))?,
        );

        Ok(Some(Reservation {
            envelope: env,
            token: reservation_token,
        }))
    }

    async fn ack(&self, token: &ReservationToken) -> Result<(), FrameworkError> {
        let stmt = Statement::from_sql_and_values(
            self.backend(),
            format!("DELETE FROM {} WHERE reserved_token = ?", self.table),
            vec![sea_orm::Value::from(token.0.to_string())],
        );
        self.db
            .execute(stmt)
            .await
            .map_err(|e| FrameworkError::internal(format!("queue ack: {e}")))?;
        Ok(())
    }

    async fn nack(
        &self,
        token: &ReservationToken,
        requeue_delay: Duration,
    ) -> Result<(), FrameworkError> {
        let now = Utc::now().timestamp();
        let new_available = now + requeue_delay.as_secs() as i64;

        // Step 1: Read the stored envelope.
        let select_sql = format!(
            "SELECT envelope_json FROM {} WHERE reserved_token = ?",
            self.table
        );
        let row = self
            .db
            .query_one(Statement::from_sql_and_values(
                self.backend(),
                &select_sql,
                vec![sea_orm::Value::from(token.0.to_string())],
            ))
            .await
            .map_err(|e| FrameworkError::internal(format!("queue nack lookup: {e}")))?;

        // Idempotent on unknown token.
        let Some(row) = row else {
            return Ok(());
        };

        let envelope_json: String = row
            .try_get_by_index::<String>(0)
            .map_err(|e| FrameworkError::internal(format!("queue envelope col: {e}")))?;

        // Step 2: Bump attempts in Rust, update available_at, write back.
        let mut env = Envelope::from_json(&envelope_json)
            .map_err(|e| FrameworkError::internal(format!("envelope decode: {e}")))?;
        env.attempts += 1;
        env.available_at = chrono::DateTime::<Utc>::from_timestamp(new_available, 0)
            .ok_or_else(|| FrameworkError::internal("nack: invalid timestamp"))?;
        let new_json = env
            .to_json()
            .map_err(|e| FrameworkError::internal(format!("envelope encode: {e}")))?;

        // Step 3: Clear reservation, update available_at, bump attempts column, write new JSON.
        let stmt = Statement::from_sql_and_values(
            self.backend(),
            format!(
                "UPDATE {} \
                 SET reserved_until = NULL, reserved_token = NULL, \
                     available_at = ?, attempts = attempts + 1, \
                     envelope_json = ? \
                 WHERE reserved_token = ?",
                self.table
            ),
            vec![
                sea_orm::Value::from(new_available),
                sea_orm::Value::from(new_json),
                sea_orm::Value::from(token.0.to_string()),
            ],
        );
        self.db
            .execute(stmt)
            .await
            .map_err(|e| FrameworkError::internal(format!("queue nack: {e}")))?;
        Ok(())
    }

    fn name(&self) -> &'static str {
        "database"
    }
}
