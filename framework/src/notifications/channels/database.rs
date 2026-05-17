//! Database notification channel — writes one row per notification.
//!
//! Persists notifications to the `notifications` table created by the
//! `20260516_create_notifications_table.sql` migration. Each delivery is
//! one `INSERT`: a fresh UUID id, the notification name as `type`, the
//! `notifiable_type` registered at construction (the model's table /
//! class name, e.g. `"users"`), the recipient route as `notifiable_id`,
//! and the JSON-encoded payload. `read_at` starts `NULL`; consumers flip
//! it when the recipient acks the notification.

use crate::error::FrameworkError;
use crate::notifications::{Channel, DynNotification};
use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use uuid::Uuid;

/// Notification channel that persists each delivery as one row in the
/// `notifications` table.
///
/// `notifiable_type` is the table / class name of the recipient model
/// (e.g. `"users"`); the channel pairs it with the route returned by
/// `Notifiable::route_for("database")` (typically the recipient's id
/// stringified) to form the polymorphic recipient reference.
pub struct DatabaseChannel {
    db: DatabaseConnection,
    notifiable_type: String,
}

impl DatabaseChannel {
    pub fn new(db: DatabaseConnection, notifiable_type: impl Into<String>) -> Self {
        Self {
            db,
            notifiable_type: notifiable_type.into(),
        }
    }
}

#[async_trait]
impl Channel for DatabaseChannel {
    fn name(&self) -> &'static str {
        "database"
    }

    async fn deliver(
        &self,
        route: &str,
        notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().naive_utc();
        let data_json = serde_json::to_string(&notification.data())
            .map_err(|e| FrameworkError::internal(format!("DatabaseChannel encode: {e}")))?;

        let stmt = Statement::from_sql_and_values(
            self.db.get_database_backend(),
            "INSERT INTO notifications (id, type, notifiable_type, notifiable_id, data, read_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, NULL, ?, ?)",
            [
                id.into(),
                notification.name().to_string().into(),
                self.notifiable_type.clone().into(),
                route.to_string().into(),
                data_json.into(),
                now.into(),
                now.into(),
            ],
        );
        self.db
            .execute(stmt)
            .await
            .map_err(|e| FrameworkError::internal(format!("DatabaseChannel insert: {e}")))?;
        Ok(())
    }
}
