//! Read-side helpers for the `notifications` table.
//!
//! The write half lives in
//! [`crate::notifications::channels::database::DatabaseChannel`]. This
//! module provides the Laravel-equivalent read surface — fetch
//! all/unread/read rows for a notifiable, mark as read/unread, and the
//! mass-update + delete helpers — without forcing every consumer to write
//! the SQL themselves.
//!
//! Laravel exposes these through the `Notifiable` + `HasDatabaseNotifications`
//! traits returning Eloquent relationships. Suprnova's `Notifiable` trait is
//! intentionally minimal (just `route_for`), so the read surface ships as
//! free functions on the framework side that take an explicit
//! `(notifiable_type, notifiable_id)` pair — the same polymorphic pair
//! [`DatabaseChannel`](crate::notifications::channels::database::DatabaseChannel)
//! writes.

use crate::error::FrameworkError;
use chrono::Utc;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, FromQueryResult, QueryResult, Statement, Value,
};

/// One persisted notification row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredNotification {
    pub id: String,
    /// Notification type — the `Notification::notification_name()` of the
    /// originating notification.
    pub type_name: String,
    /// Recipient model name (e.g. `"users"`).
    pub notifiable_type: String,
    /// Recipient id, stringified.
    pub notifiable_id: String,
    /// JSON-decoded data column.
    pub data: serde_json::Value,
    /// `Some(t)` iff the recipient has marked the notification read.
    pub read_at: Option<chrono::NaiveDateTime>,
    pub created_at: chrono::NaiveDateTime,
    pub updated_at: chrono::NaiveDateTime,
}

impl FromQueryResult for StoredNotification {
    fn from_query_result(res: &QueryResult, _pre: &str) -> Result<Self, sea_orm::DbErr> {
        let data_text: String = res.try_get("", "data")?;
        let data: serde_json::Value =
            serde_json::from_str(&data_text).map_err(|e| sea_orm::DbErr::Custom(e.to_string()))?;
        Ok(Self {
            id: res.try_get("", "id")?,
            type_name: res.try_get("", "type")?,
            notifiable_type: res.try_get("", "notifiable_type")?,
            notifiable_id: res.try_get("", "notifiable_id")?,
            data,
            read_at: res.try_get("", "read_at").ok(),
            created_at: res.try_get("", "created_at")?,
            updated_at: res.try_get("", "updated_at")?,
        })
    }
}

const COLS: &str =
    "id, type, notifiable_type, notifiable_id, data, read_at, created_at, updated_at";

async fn run(
    db: &DatabaseConnection,
    sql: &str,
    values: Vec<Value>,
) -> Result<Vec<StoredNotification>, FrameworkError> {
    let stmt = Statement::from_sql_and_values(db.get_database_backend(), sql, values);
    let rows = db
        .query_all(stmt)
        .await
        .map_err(|e| FrameworkError::internal(format!("notifications read: {e}")))?;
    rows.into_iter()
        .map(|r| {
            StoredNotification::from_query_result(&r, "")
                .map_err(|e| FrameworkError::internal(format!("notifications decode: {e}")))
        })
        .collect()
}

/// All notifications for a recipient, newest first. Laravel's
/// `$user->notifications` equivalent.
pub async fn all_for(
    db: &DatabaseConnection,
    notifiable_type: &str,
    notifiable_id: &str,
) -> Result<Vec<StoredNotification>, FrameworkError> {
    let sql = format!(
        "SELECT {COLS} FROM notifications \
         WHERE notifiable_type = ? AND notifiable_id = ? \
         ORDER BY created_at DESC"
    );
    run(db, &sql, vec![notifiable_type.into(), notifiable_id.into()]).await
}

/// Unread notifications (`read_at IS NULL`) for a recipient, newest first.
/// Laravel's `$user->unreadNotifications`.
pub async fn unread_for(
    db: &DatabaseConnection,
    notifiable_type: &str,
    notifiable_id: &str,
) -> Result<Vec<StoredNotification>, FrameworkError> {
    let sql = format!(
        "SELECT {COLS} FROM notifications \
         WHERE notifiable_type = ? AND notifiable_id = ? AND read_at IS NULL \
         ORDER BY created_at DESC"
    );
    run(db, &sql, vec![notifiable_type.into(), notifiable_id.into()]).await
}

/// Read notifications (`read_at IS NOT NULL`) for a recipient, newest first.
/// Laravel's `$user->readNotifications`.
pub async fn read_for(
    db: &DatabaseConnection,
    notifiable_type: &str,
    notifiable_id: &str,
) -> Result<Vec<StoredNotification>, FrameworkError> {
    let sql = format!(
        "SELECT {COLS} FROM notifications \
         WHERE notifiable_type = ? AND notifiable_id = ? AND read_at IS NOT NULL \
         ORDER BY created_at DESC"
    );
    run(db, &sql, vec![notifiable_type.into(), notifiable_id.into()]).await
}

/// Mark a single notification row as read. No-op if `read_at` is already set
/// (matches Laravel's `markAsRead` idempotence).
pub async fn mark_as_read(db: &DatabaseConnection, id: &str) -> Result<(), FrameworkError> {
    let now = Utc::now().naive_utc();
    let stmt = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE notifications SET read_at = ?, updated_at = ? \
         WHERE id = ? AND read_at IS NULL",
        vec![now.into(), now.into(), id.into()],
    );
    db.execute(stmt)
        .await
        .map_err(|e| FrameworkError::internal(format!("mark_as_read: {e}")))?;
    Ok(())
}

/// Mark a single notification row as unread. No-op if `read_at` is already
/// NULL (matches Laravel's `markAsUnread` idempotence).
pub async fn mark_as_unread(db: &DatabaseConnection, id: &str) -> Result<(), FrameworkError> {
    let now = Utc::now().naive_utc();
    let stmt = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE notifications SET read_at = NULL, updated_at = ? \
         WHERE id = ? AND read_at IS NOT NULL",
        vec![now.into(), id.into()],
    );
    db.execute(stmt)
        .await
        .map_err(|e| FrameworkError::internal(format!("mark_as_unread: {e}")))?;
    Ok(())
}

/// Mass-mark every unread notification for the recipient as read. Laravel's
/// `$user->unreadNotifications->markAsRead()` equivalent. Returns the
/// number of rows updated.
pub async fn mark_all_as_read(
    db: &DatabaseConnection,
    notifiable_type: &str,
    notifiable_id: &str,
) -> Result<u64, FrameworkError> {
    let now = Utc::now().naive_utc();
    let stmt = Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE notifications SET read_at = ?, updated_at = ? \
         WHERE notifiable_type = ? AND notifiable_id = ? AND read_at IS NULL",
        vec![
            now.into(),
            now.into(),
            notifiable_type.into(),
            notifiable_id.into(),
        ],
    );
    let res = db
        .execute(stmt)
        .await
        .map_err(|e| FrameworkError::internal(format!("mark_all_as_read: {e}")))?;
    Ok(res.rows_affected())
}

/// Delete every notification row for the recipient. Returns the number of
/// rows deleted. Laravel's `$user->notifications()->delete()`.
pub async fn delete_for(
    db: &DatabaseConnection,
    notifiable_type: &str,
    notifiable_id: &str,
) -> Result<u64, FrameworkError> {
    let stmt = Statement::from_sql_and_values(
        db.get_database_backend(),
        "DELETE FROM notifications WHERE notifiable_type = ? AND notifiable_id = ?",
        vec![notifiable_type.into(), notifiable_id.into()],
    );
    let res = db
        .execute(stmt)
        .await
        .map_err(|e| FrameworkError::internal(format!("delete_for: {e}")))?;
    Ok(res.rows_affected())
}
