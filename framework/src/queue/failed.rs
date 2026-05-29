//! Failed-job storage.
//!
//! When the worker dead-letters a job (max_tries exhausted, fatal timeout,
//! manual fail), the [`FailedJobStore`] receives a record carrying the
//! envelope + the failure cause. Records can be listed, retried, forgotten,
//! flushed. Mirrors Laravel 13's `Illuminate\Queue\Failed\*`.
//!
//! Three backends ship:
//! - [`MemoryFailedJobStore`] — in-process Vec, lost on restart. Default
//!   wired by `bootstrap_default`.
//! - [`DatabaseFailedJobStore`] — persists to a `failed_jobs` table via
//!   SeaORM. Production default for the database driver.
//! - [`NullFailedJobStore`] — discards every record. Mirrors Laravel's
//!   `NullFailedJobProvider`.
//!
//! Configure via [`Queue::set_failed_store`](crate::queue::Queue::set_failed_store)
//! at boot.

use crate::database::validate_identifier;
use crate::error::FrameworkError;
use crate::queue::envelope::Envelope;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// One persisted failed-job record. The serialized envelope is held verbatim
/// so an operator running `queue:retry <id>` can re-enqueue the exact
/// payload that originally failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedJob {
    pub id: Uuid,
    pub connection: String,
    pub queue: String,
    pub job_name: String,
    pub envelope_json: String,
    pub exception: String,
    pub failed_at: DateTime<Utc>,
}

#[async_trait]
pub trait FailedJobStore: Send + Sync {
    /// Persist a new failed-job record. Returns the record's id.
    async fn log(
        &self,
        connection: &str,
        queue: &str,
        env: &Envelope,
        exception: &str,
    ) -> Result<Uuid, FrameworkError>;

    /// All records, newest first.
    async fn all(&self) -> Result<Vec<FailedJob>, FrameworkError>;

    /// IDs only, newest first. Mirrors Laravel's `ids($queue)`.
    async fn ids(&self) -> Result<Vec<Uuid>, FrameworkError>;

    /// Find a single record by id.
    async fn find(&self, id: Uuid) -> Result<Option<FailedJob>, FrameworkError>;

    /// Drop a single record. Returns `true` if a row was removed.
    async fn forget(&self, id: Uuid) -> Result<bool, FrameworkError>;

    /// Drop every record (optionally only those older than `before`).
    /// Returns the number of records dropped.
    async fn flush(&self, before: Option<DateTime<Utc>>) -> Result<u64, FrameworkError>;

    /// Number of records. Mirrors Laravel's `count()`.
    async fn count(&self) -> Result<u64, FrameworkError>;
}

// ---------------------------------------------------------------------------
// Memory backend
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct MemoryFailedJobStore {
    rows: Mutex<Vec<FailedJob>>,
}

impl MemoryFailedJobStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl FailedJobStore for MemoryFailedJobStore {
    async fn log(
        &self,
        connection: &str,
        queue: &str,
        env: &Envelope,
        exception: &str,
    ) -> Result<Uuid, FrameworkError> {
        let id = Uuid::new_v4();
        let envelope_json = env.to_json().map_err(|e| {
            FrameworkError::internal(format!("encode envelope for failed_jobs: {e}"))
        })?;
        let row = FailedJob {
            id,
            connection: connection.into(),
            queue: queue.into(),
            job_name: env.job_name.clone(),
            envelope_json,
            exception: exception.into(),
            failed_at: Utc::now(),
        };
        let mut g = self
            .rows
            .lock()
            .map_err(|_| FrameworkError::internal("failed_jobs store poisoned"))?;
        g.push(row);
        Ok(id)
    }

    async fn all(&self) -> Result<Vec<FailedJob>, FrameworkError> {
        let g = self
            .rows
            .lock()
            .map_err(|_| FrameworkError::internal("failed_jobs store poisoned"))?;
        let mut v = g.clone();
        v.sort_by_key(|r| std::cmp::Reverse(r.failed_at));
        Ok(v)
    }

    async fn ids(&self) -> Result<Vec<Uuid>, FrameworkError> {
        Ok(self.all().await?.into_iter().map(|r| r.id).collect())
    }

    async fn find(&self, id: Uuid) -> Result<Option<FailedJob>, FrameworkError> {
        let g = self
            .rows
            .lock()
            .map_err(|_| FrameworkError::internal("failed_jobs store poisoned"))?;
        Ok(g.iter().find(|r| r.id == id).cloned())
    }

    async fn forget(&self, id: Uuid) -> Result<bool, FrameworkError> {
        let mut g = self
            .rows
            .lock()
            .map_err(|_| FrameworkError::internal("failed_jobs store poisoned"))?;
        let before = g.len();
        g.retain(|r| r.id != id);
        Ok(g.len() < before)
    }

    async fn flush(&self, before: Option<DateTime<Utc>>) -> Result<u64, FrameworkError> {
        let mut g = self
            .rows
            .lock()
            .map_err(|_| FrameworkError::internal("failed_jobs store poisoned"))?;
        let before_len = g.len();
        match before {
            Some(cutoff) => g.retain(|r| r.failed_at >= cutoff),
            None => g.clear(),
        }
        Ok((before_len - g.len()) as u64)
    }

    async fn count(&self) -> Result<u64, FrameworkError> {
        let g = self
            .rows
            .lock()
            .map_err(|_| FrameworkError::internal("failed_jobs store poisoned"))?;
        Ok(g.len() as u64)
    }
}

// ---------------------------------------------------------------------------
// Null backend
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct NullFailedJobStore;

impl NullFailedJobStore {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl FailedJobStore for NullFailedJobStore {
    async fn log(
        &self,
        _connection: &str,
        _queue: &str,
        _env: &Envelope,
        _exception: &str,
    ) -> Result<Uuid, FrameworkError> {
        Ok(Uuid::new_v4())
    }
    async fn all(&self) -> Result<Vec<FailedJob>, FrameworkError> {
        Ok(vec![])
    }
    async fn ids(&self) -> Result<Vec<Uuid>, FrameworkError> {
        Ok(vec![])
    }
    async fn find(&self, _id: Uuid) -> Result<Option<FailedJob>, FrameworkError> {
        Ok(None)
    }
    async fn forget(&self, _id: Uuid) -> Result<bool, FrameworkError> {
        Ok(false)
    }
    async fn flush(&self, _before: Option<DateTime<Utc>>) -> Result<u64, FrameworkError> {
        Ok(0)
    }
    async fn count(&self) -> Result<u64, FrameworkError> {
        Ok(0)
    }
}

// ---------------------------------------------------------------------------
// Database backend
// ---------------------------------------------------------------------------

/// SeaORM-backed failed-job store. Schema (operator-managed):
///
/// ```sql
/// CREATE TABLE failed_jobs (
///     id              TEXT PRIMARY KEY,
///     connection      TEXT NOT NULL,
///     queue           TEXT NOT NULL,
///     job_name        TEXT NOT NULL,
///     envelope_json   TEXT NOT NULL,
///     exception       TEXT NOT NULL,
///     failed_at       INTEGER NOT NULL
/// );
/// CREATE INDEX idx_failed_jobs_failed_at ON failed_jobs(failed_at);
/// ```
///
/// The `table` argument is validated as a SQL identifier once at construction
/// (same shape as [`crate::queue::database::DatabaseQueueDriver::new`]).
pub struct DatabaseFailedJobStore {
    db: DatabaseConnection,
    table: String,
}

impl DatabaseFailedJobStore {
    pub fn new(db: DatabaseConnection, table: String) -> Result<Self, FrameworkError> {
        validate_identifier(&table)?;
        Ok(Self { db, table })
    }

    fn backend(&self) -> DatabaseBackend {
        self.db.get_database_backend()
    }
}

#[async_trait]
impl FailedJobStore for DatabaseFailedJobStore {
    async fn log(
        &self,
        connection: &str,
        queue: &str,
        env: &Envelope,
        exception: &str,
    ) -> Result<Uuid, FrameworkError> {
        let id = Uuid::new_v4();
        let envelope_json = env.to_json().map_err(|e| {
            FrameworkError::internal(format!("encode envelope for failed_jobs: {e}"))
        })?;
        let stmt = Statement::from_sql_and_values(
            self.backend(),
            format!(
                "INSERT INTO {} (id, connection, queue, job_name, envelope_json, exception, failed_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                self.table
            ),
            vec![
                sea_orm::Value::from(id.to_string()),
                sea_orm::Value::from(connection.to_string()),
                sea_orm::Value::from(queue.to_string()),
                sea_orm::Value::from(env.job_name.clone()),
                sea_orm::Value::from(envelope_json),
                sea_orm::Value::from(exception.to_string()),
                sea_orm::Value::from(Utc::now().timestamp()),
            ],
        );
        self.db
            .execute(stmt)
            .await
            .map_err(|e| FrameworkError::internal(format!("failed_jobs insert: {e}")))?;
        Ok(id)
    }

    async fn all(&self) -> Result<Vec<FailedJob>, FrameworkError> {
        let rows = self
            .db
            .query_all(Statement::from_string(
                self.backend(),
                format!(
                    "SELECT id, connection, queue, job_name, envelope_json, exception, failed_at \
                     FROM {} ORDER BY failed_at DESC",
                    self.table
                ),
            ))
            .await
            .map_err(|e| FrameworkError::internal(format!("failed_jobs select: {e}")))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(decode_row(&row)?);
        }
        Ok(out)
    }

    async fn ids(&self) -> Result<Vec<Uuid>, FrameworkError> {
        Ok(self.all().await?.into_iter().map(|r| r.id).collect())
    }

    async fn find(&self, id: Uuid) -> Result<Option<FailedJob>, FrameworkError> {
        let stmt = Statement::from_sql_and_values(
            self.backend(),
            format!(
                "SELECT id, connection, queue, job_name, envelope_json, exception, failed_at \
                 FROM {} WHERE id = ?",
                self.table
            ),
            vec![sea_orm::Value::from(id.to_string())],
        );
        let row = self
            .db
            .query_one(stmt)
            .await
            .map_err(|e| FrameworkError::internal(format!("failed_jobs find: {e}")))?;
        match row {
            Some(r) => Ok(Some(decode_row(&r)?)),
            None => Ok(None),
        }
    }

    async fn forget(&self, id: Uuid) -> Result<bool, FrameworkError> {
        let stmt = Statement::from_sql_and_values(
            self.backend(),
            format!("DELETE FROM {} WHERE id = ?", self.table),
            vec![sea_orm::Value::from(id.to_string())],
        );
        let r = self
            .db
            .execute(stmt)
            .await
            .map_err(|e| FrameworkError::internal(format!("failed_jobs forget: {e}")))?;
        Ok(r.rows_affected() > 0)
    }

    async fn flush(&self, before: Option<DateTime<Utc>>) -> Result<u64, FrameworkError> {
        let stmt = match before {
            Some(cutoff) => Statement::from_sql_and_values(
                self.backend(),
                format!("DELETE FROM {} WHERE failed_at < ?", self.table),
                vec![sea_orm::Value::from(cutoff.timestamp())],
            ),
            None => Statement::from_string(self.backend(), format!("DELETE FROM {}", self.table)),
        };
        let r = self
            .db
            .execute(stmt)
            .await
            .map_err(|e| FrameworkError::internal(format!("failed_jobs flush: {e}")))?;
        Ok(r.rows_affected())
    }

    async fn count(&self) -> Result<u64, FrameworkError> {
        let row = self
            .db
            .query_one(Statement::from_string(
                self.backend(),
                format!("SELECT COUNT(*) FROM {}", self.table),
            ))
            .await
            .map_err(|e| FrameworkError::internal(format!("failed_jobs count: {e}")))?;
        let n: i64 = match row {
            Some(r) => r
                .try_get_by_index(0)
                .map_err(|e| FrameworkError::internal(format!("failed_jobs count col: {e}")))?,
            None => 0,
        };
        Ok(n.max(0) as u64)
    }
}

fn decode_row(row: &sea_orm::QueryResult) -> Result<FailedJob, FrameworkError> {
    let id_s: String = row
        .try_get_by_index(0)
        .map_err(|e| FrameworkError::internal(format!("failed_jobs id col: {e}")))?;
    let id = Uuid::parse_str(&id_s)
        .map_err(|e| FrameworkError::internal(format!("failed_jobs id parse: {e}")))?;
    let connection: String = row
        .try_get_by_index(1)
        .map_err(|e| FrameworkError::internal(format!("failed_jobs connection col: {e}")))?;
    let queue: String = row
        .try_get_by_index(2)
        .map_err(|e| FrameworkError::internal(format!("failed_jobs queue col: {e}")))?;
    let job_name: String = row
        .try_get_by_index(3)
        .map_err(|e| FrameworkError::internal(format!("failed_jobs job_name col: {e}")))?;
    let envelope_json: String = row
        .try_get_by_index(4)
        .map_err(|e| FrameworkError::internal(format!("failed_jobs envelope_json col: {e}")))?;
    let exception: String = row
        .try_get_by_index(5)
        .map_err(|e| FrameworkError::internal(format!("failed_jobs exception col: {e}")))?;
    let failed_at_ts: i64 = row
        .try_get_by_index(6)
        .map_err(|e| FrameworkError::internal(format!("failed_jobs failed_at col: {e}")))?;
    let failed_at = DateTime::<Utc>::from_timestamp(failed_at_ts, 0)
        .ok_or_else(|| FrameworkError::internal("failed_jobs failed_at: invalid timestamp"))?;
    Ok(FailedJob {
        id,
        connection,
        queue,
        job_name,
        envelope_json,
        exception,
        failed_at,
    })
}

// ---------------------------------------------------------------------------
// Global registration
// ---------------------------------------------------------------------------

use std::sync::RwLock;

static STORE: RwLock<Option<Arc<dyn FailedJobStore>>> = RwLock::new(None);

pub(crate) fn install(store: Arc<dyn FailedJobStore>) {
    if let Ok(mut g) = STORE.write() {
        *g = Some(store);
    }
}

pub(crate) fn current() -> Option<Arc<dyn FailedJobStore>> {
    STORE.read().ok().and_then(|g| g.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{BackoffSchedule, CURRENT_SCHEMA_VERSION};

    fn env(name: &str) -> Envelope {
        Envelope {
            schema_version: CURRENT_SCHEMA_VERSION,
            id: Uuid::new_v4(),
            job_name: name.into(),
            payload: serde_json::json!({}),
            dispatched_at: Utc::now(),
            available_at: Utc::now(),
            attempts: 0,
            max_tries: 3,
            backoff: BackoffSchedule::default(),
            timeout_secs: None,
            fail_on_timeout: false,
            idempotency_key: None,
            batch_id: None,
            chain_remaining: Vec::new(),
        }
    }

    #[tokio::test]
    async fn memory_store_round_trips_records() {
        let store = MemoryFailedJobStore::new();
        let id = store
            .log("default", "default", &env("A"), "boom")
            .await
            .unwrap();
        let id2 = store
            .log("default", "default", &env("B"), "kaboom")
            .await
            .unwrap();
        assert_eq!(store.count().await.unwrap(), 2);
        let all = store.all().await.unwrap();
        assert_eq!(all.len(), 2);
        assert!(store.find(id).await.unwrap().is_some());
        assert!(store.forget(id).await.unwrap());
        assert_eq!(store.count().await.unwrap(), 1);
        assert_eq!(store.flush(None).await.unwrap(), 1);
        assert_eq!(store.count().await.unwrap(), 0);
        // forget on missing id is false
        assert!(!store.forget(id2).await.unwrap());
    }

    #[tokio::test]
    async fn null_store_is_inert() {
        let store = NullFailedJobStore::new();
        let _ = store
            .log("default", "default", &env("A"), "boom")
            .await
            .unwrap();
        assert_eq!(store.count().await.unwrap(), 0);
        assert!(store.all().await.unwrap().is_empty());
    }
}
