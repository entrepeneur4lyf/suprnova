use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::notifications::channels::database::DatabaseChannel;
use suprnova::notifications::{Channel, Notifiable, Notification, NotificationDispatcher};

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
        if channel == "database" {
            Some(self.id.to_string())
        } else {
            None
        }
    }
}

// Source-of-truth migration: embed at compile time so the test's schema
// can never drift from production. SQLite's `execute_unprepared` runs a
// single statement per call, and the migration's leading comment block
// contains prose semicolons that would corrupt a naive `split(';')`. So
// we first strip `--` line comments, then split on `;` and run each
// non-empty trimmed statement.
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

#[tokio::test]
#[serial]
async fn database_channel_inserts_notifications_row() {
    let db = fresh_db().await;
    let channel: Arc<dyn Channel> = Arc::new(DatabaseChannel::new(db.clone(), "users"));
    let dispatcher = NotificationDispatcher::new().register_channel(channel);

    dispatcher
        .notify(
            &User { id: 42 },
            &OrderShipped {
                tracking: "1Z999".into(),
            },
        )
        .await
        .unwrap();

    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT type, notifiable_type, notifiable_id, data FROM notifications".to_string(),
        ))
        .await
        .unwrap()
        .expect("notifications row inserted");
    assert_eq!(row.try_get_by_index::<String>(0).unwrap(), "OrderShipped");
    assert_eq!(row.try_get_by_index::<String>(1).unwrap(), "users");
    assert_eq!(row.try_get_by_index::<String>(2).unwrap(), "42");
    let data: String = row.try_get_by_index(3).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(parsed["tracking"], "1Z999");
}
