//! Coverage for the database read-model helpers (`all_for` / `unread_for` /
//! `read_for` / `mark_as_read` / `mark_as_unread` / `mark_all_as_read` /
//! `delete_for`).
//!
//! Uses an in-memory SQLite via the same shared migration as
//! `notification_database.rs`.

use sea_orm::{ConnectionTrait, Database, DatabaseConnection};
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::notifications::channels::database::DatabaseChannel;
use suprnova::notifications::{
    Channel, Notifiable, Notification, NotificationDispatcher, all_for, delete_for,
    mark_all_as_read, mark_as_read, mark_as_unread, read_for, unread_for,
};

const NOTIFICATIONS_MIGRATION: &str =
    include_str!("../migrations/20260516_create_notifications_table.sql");

fn strip_sql_line_comments(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("--") {
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn fresh_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let cleaned = strip_sql_line_comments(NOTIFICATIONS_MIGRATION);
    for stmt in cleaned.split(';') {
        let trimmed = stmt.trim();
        if trimmed.is_empty() {
            continue;
        }
        db.execute_unprepared(trimmed).await.unwrap();
    }
    db
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct OrderShipped {
    tracking: String,
}

impl Notification for OrderShipped {
    fn notification_name() -> &'static str {
        "OrderShipped"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["database"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "tracking": self.tracking })
    }
}

struct User {
    id: i64,
}

impl Notifiable for User {
    fn route_for(&self, channel: &str) -> Option<String> {
        (channel == "database").then(|| self.id.to_string())
    }
}

/// Insert N notifications for `(notifiable_type, notifiable_id)` and return
/// their ids in insertion order.
async fn seed_n(db: &DatabaseConnection, user_id: i64, n: usize) -> Vec<String> {
    let channel: Arc<dyn Channel> = Arc::new(DatabaseChannel::new(db.clone(), "users"));
    let dispatcher = NotificationDispatcher::new().register_channel(channel);
    for i in 0..n {
        dispatcher
            .notify(
                &User { id: user_id },
                &OrderShipped {
                    tracking: format!("T{i}"),
                },
            )
            .await
            .unwrap();
    }
    let rows = all_for(db, "users", &user_id.to_string()).await.unwrap();
    rows.iter().map(|r| r.id.clone()).collect()
}

#[tokio::test]
#[serial]
async fn all_for_returns_every_row_newest_first() {
    let db = fresh_db().await;
    let _ids = seed_n(&db, 1, 3).await;
    let rows = all_for(&db, "users", "1").await.unwrap();
    assert_eq!(rows.len(), 3);
    // All belong to this user.
    for r in &rows {
        assert_eq!(r.notifiable_type, "users");
        assert_eq!(r.notifiable_id, "1");
        assert_eq!(r.type_name, "OrderShipped");
    }
    // newest first → r[0].created_at >= r[2].created_at
    assert!(rows[0].created_at >= rows[2].created_at);
}

#[tokio::test]
#[serial]
async fn unread_for_excludes_read_and_read_for_excludes_unread() {
    let db = fresh_db().await;
    let ids = seed_n(&db, 2, 3).await;

    mark_as_read(&db, &ids[0]).await.unwrap();
    let unread = unread_for(&db, "users", "2").await.unwrap();
    let read = read_for(&db, "users", "2").await.unwrap();
    assert_eq!(unread.len(), 2);
    assert_eq!(read.len(), 1);
    assert!(read[0].read_at.is_some());
    assert!(unread.iter().all(|r| r.read_at.is_none()));
}

#[tokio::test]
#[serial]
async fn mark_as_read_is_idempotent_and_round_trips() {
    let db = fresh_db().await;
    let ids = seed_n(&db, 3, 1).await;
    let id = &ids[0];
    mark_as_read(&db, id).await.unwrap();
    mark_as_read(&db, id).await.unwrap(); // idempotent no-op
    let read = read_for(&db, "users", "3").await.unwrap();
    assert_eq!(read.len(), 1);

    mark_as_unread(&db, id).await.unwrap();
    mark_as_unread(&db, id).await.unwrap(); // idempotent no-op
    let unread = unread_for(&db, "users", "3").await.unwrap();
    assert_eq!(unread.len(), 1);
}

#[tokio::test]
#[serial]
async fn mark_all_as_read_marks_only_target_recipient() {
    let db = fresh_db().await;
    seed_n(&db, 4, 2).await;
    seed_n(&db, 5, 1).await;
    let n = mark_all_as_read(&db, "users", "4").await.unwrap();
    assert_eq!(n, 2);
    assert_eq!(unread_for(&db, "users", "4").await.unwrap().len(), 0);
    // Recipient 5 untouched.
    assert_eq!(unread_for(&db, "users", "5").await.unwrap().len(), 1);
}

#[tokio::test]
#[serial]
async fn delete_for_scopes_to_recipient() {
    let db = fresh_db().await;
    seed_n(&db, 6, 3).await;
    seed_n(&db, 7, 2).await;
    let n = delete_for(&db, "users", "6").await.unwrap();
    assert_eq!(n, 3);
    assert_eq!(all_for(&db, "users", "6").await.unwrap().len(), 0);
    assert_eq!(all_for(&db, "users", "7").await.unwrap().len(), 2);
}
