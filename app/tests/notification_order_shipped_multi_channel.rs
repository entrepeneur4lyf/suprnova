//! Polish Item 2 — multi-channel `OrderShipped` end-to-end dogfood.
//!
//! Sets up real channels: `MailChannel` against an `InMemoryMailTransport`
//! plus `DatabaseChannel` against an in-memory sqlite that has been
//! migrated with the production notifications schema. A single
//! `Notify::send` call must fan out to both channels — the mail must
//! land in the transport's capture buffer with the rendered subject/
//! body produced by `OrderShipped::to_mail`, AND a row must land in
//! the `notifications` table.
//!
//! Pins the full multi-channel story end-to-end so a future regression
//! (channel registration ordering, renderer lookup, route plumbing,
//! schema drift) surfaces as a failing test rather than a silent
//! single-channel delivery.
//!
//! Marked `#[serial]` because we mutate three globals: the mail
//! transport, the notification dispatcher, and the renderer registry.

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use serial_test::serial;
use std::sync::Arc;
use suprnova::mail::Mail;
use suprnova::mail::memory::InMemoryMailTransport;
use suprnova::notifications::{Notifiable, NotificationDispatcher, Notify};
use suprnova::{DatabaseChannel, MailChannel};

use app::notifications::order_shipped::OrderShipped;

struct User {
    id: i64,
    email: String,
}

impl Notifiable for User {
    fn route_for(&self, channel: &str) -> Option<String> {
        match channel {
            "mail" => Some(self.email.clone()),
            "database" => Some(self.id.to_string()),
            _ => None,
        }
    }
}

const NOTIFICATIONS_MIGRATION: &str =
    include_str!("../../framework/migrations/20260516_create_notifications_table.sql");

// Naive — truncates at the first `--` in each line. Safe ONLY because
// the embedded migration has no quoted string literals containing
// "--". Mirrors `framework/tests/notification_database.rs`.
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
        db.execute_unprepared(trimmed)
            .await
            .expect("notifications migration applies cleanly");
    }
    db
}

#[tokio::test]
#[serial]
async fn order_shipped_dispatches_to_mail_and_database() {
    // Bind globals.
    let transport = Arc::new(InMemoryMailTransport::new());
    let _ = Mail::set_transport(transport.clone());

    let db = fresh_db().await;

    let _ = suprnova::register_mail_renderer::<OrderShipped>();

    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(MailChannel::new()))
        .register_channel(Arc::new(DatabaseChannel::new(db.clone(), "users")));
    let _ = suprnova::notifications::set_dispatcher(Arc::new(dispatcher));

    // One Notify::send — fans out across both registered channels.
    Notify::send(
        &User {
            id: 42,
            email: "alice@example.org".into(),
        },
        &OrderShipped {
            tracking: "1Z999AA10123456784".into(),
        },
    )
    .await
    .unwrap();

    // Mail channel fired with the rendered subject + body.
    let msgs = transport.captured();
    assert_eq!(msgs.len(), 1, "exactly one mail captured");
    assert_eq!(msgs[0].to.len(), 1, "single recipient");
    assert_eq!(msgs[0].to[0].email, "alice@example.org");
    assert_eq!(msgs[0].from.email, "orders@suprnova.dev");
    assert!(
        msgs[0].subject.contains("1Z999AA10123456784"),
        "subject carries the tracking number: {}",
        msgs[0].subject
    );
    assert!(
        msgs[0]
            .text
            .as_deref()
            .map(|t| t.contains("1Z999AA10123456784"))
            .unwrap_or(false),
        "text body carries the tracking number: {:?}",
        msgs[0].text
    );
    assert!(
        msgs[0]
            .html
            .as_deref()
            .map(|h| h.contains("1Z999AA10123456784"))
            .unwrap_or(false),
        "html body carries the tracking number: {:?}",
        msgs[0].html
    );

    // Database channel fired with the canonical envelope.
    let row = db
        .query_one(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT type, notifiable_type, notifiable_id, data FROM notifications",
        ))
        .await
        .unwrap()
        .expect("notifications row inserted");
    assert_eq!(row.try_get_by_index::<String>(0).unwrap(), "OrderShipped");
    assert_eq!(row.try_get_by_index::<String>(1).unwrap(), "users");
    assert_eq!(row.try_get_by_index::<String>(2).unwrap(), "42");
    let data: String = row.try_get_by_index(3).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(parsed["tracking"], "1Z999AA10123456784");
}
