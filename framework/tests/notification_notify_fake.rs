//! Coverage for `Notify::fake()` — the in-memory recorder that captures
//! dispatches without invoking channels.

use serde::{Deserialize, Serialize};
use serial_test::serial;
use suprnova::notifications::testing::{
    assert_count, assert_nothing_sent, assert_nothing_sent_to, assert_sent_named,
    assert_sent_times, assert_sent_to, assert_sent_to_on, recorded,
};
use suprnova::notifications::{Notifiable, Notification, Notify};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct OrderShipped {
    tracking: String,
}

impl Notification for OrderShipped {
    fn notification_name() -> &'static str {
        "OrderShipped"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["mail", "database"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "tracking": self.tracking })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct WeeklyReminder;

impl Notification for WeeklyReminder {
    fn notification_name() -> &'static str {
        "WeeklyReminder"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["mail"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
}

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

#[tokio::test]
#[serial]
async fn notify_fake_records_send_per_channel() {
    let _g = Notify::fake();
    let user = User {
        id: 1,
        email: "u1@example.com".into(),
    };
    Notify::send(
        &user,
        &OrderShipped {
            tracking: "Z1".into(),
        },
    )
    .await
    .unwrap();

    // OrderShipped declares 2 channels → 2 records (one per route).
    assert_count(2);
    assert_sent_to("u1@example.com", "OrderShipped");
    assert_sent_to("1", "OrderShipped");
    assert_sent_to_on("u1@example.com", "mail", "OrderShipped");
    assert_sent_to_on("1", "database", "OrderShipped");
}

#[tokio::test]
#[serial]
async fn notify_fake_captures_payload_for_inspection() {
    let _g = Notify::fake();
    let user = User {
        id: 9,
        email: "u9@example.com".into(),
    };
    Notify::send(
        &user,
        &OrderShipped {
            tracking: "TRACK9".into(),
        },
    )
    .await
    .unwrap();

    let records = recorded();
    let mail_rec = records
        .iter()
        .find(|r| r.channel == "mail")
        .expect("mail dispatch was recorded");
    assert_eq!(mail_rec.data["tracking"], "TRACK9");
    assert_eq!(mail_rec.notification, "OrderShipped");
    assert_eq!(mail_rec.route, "u9@example.com");
}

#[tokio::test]
#[serial]
async fn notify_fake_skips_channels_with_no_route() {
    let _g = Notify::fake();
    struct EmailOnly;
    impl Notifiable for EmailOnly {
        fn route_for(&self, channel: &str) -> Option<String> {
            (channel == "mail").then(|| "x@example.com".into())
        }
    }
    Notify::send(
        &EmailOnly,
        &OrderShipped {
            tracking: "Z".into(),
        },
    )
    .await
    .unwrap();
    // database has no route — only one channel recorded.
    assert_count(1);
    assert_sent_to_on("x@example.com", "mail", "OrderShipped");
}

#[tokio::test]
#[serial]
async fn notify_fake_assert_sent_times_and_nothing_sent_to() {
    let _g = Notify::fake();
    let u = User {
        id: 7,
        email: "u7@example.com".into(),
    };
    Notify::send(&u, &WeeklyReminder).await.unwrap();
    Notify::send(&u, &WeeklyReminder).await.unwrap();
    assert_sent_times("WeeklyReminder", 2);
    assert_sent_named("WeeklyReminder");
    assert_nothing_sent_to("other@example.com");
}

#[tokio::test]
#[serial]
async fn notify_fake_assert_nothing_sent_on_clean_slate() {
    let _g = Notify::fake();
    assert_nothing_sent();
}

#[tokio::test]
#[serial]
async fn notify_fake_works_for_queue_too() {
    let _g = Notify::fake();
    // Queue::push would require a bound queue driver — under Notify::fake the
    // job is recorded and the queue is never consulted.
    let u = User {
        id: 3,
        email: "q@example.com".into(),
    };
    Notify::queue(
        &u,
        OrderShipped {
            tracking: "Q1".into(),
        },
    )
    .await
    .unwrap();
    assert_count(2); // mail + database recorded
    assert_sent_to_on("q@example.com", "mail", "OrderShipped");
}
