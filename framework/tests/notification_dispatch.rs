use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use suprnova::notifications::{Channel, DynNotification, Notifiable, Notification, NotificationDispatcher};
use suprnova::FrameworkError;
use tracing_test::traced_test;

static MAIL_HITS: AtomicU32 = AtomicU32::new(0);
static DB_HITS: AtomicU32 = AtomicU32::new(0);

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Recipient {
    id: i64,
    email: String,
}

impl Notifiable for Recipient {
    fn route_for(&self, channel: &str) -> Option<String> {
        match channel {
            "mail" => Some(self.email.clone()),
            "database" => Some(self.id.to_string()),
            _ => None,
        }
    }
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
        vec!["mail", "database"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "tracking": self.tracking })
    }
}

struct MailChannelStub;
#[async_trait]
impl Channel for MailChannelStub {
    fn name(&self) -> &'static str {
        "mail"
    }
    async fn deliver(
        &self,
        _route: &str,
        _notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        MAIL_HITS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct DbChannelStub;
#[async_trait]
impl Channel for DbChannelStub {
    fn name(&self) -> &'static str {
        "database"
    }
    async fn deliver(
        &self,
        _route: &str,
        _notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        DB_HITS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn notification_dispatches_to_each_declared_channel() {
    MAIL_HITS.store(0, Ordering::SeqCst);
    DB_HITS.store(0, Ordering::SeqCst);

    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(MailChannelStub))
        .register_channel(Arc::new(DbChannelStub));

    let recipient = Recipient {
        id: 42,
        email: "alice@example.org".into(),
    };
    let notification = OrderShipped {
        tracking: "1Z999".into(),
    };

    dispatcher.notify(&recipient, &notification).await.unwrap();

    assert_eq!(MAIL_HITS.load(Ordering::SeqCst), 1);
    assert_eq!(DB_HITS.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[serial]
async fn notification_skips_channels_without_a_route() {
    MAIL_HITS.store(0, Ordering::SeqCst);
    DB_HITS.store(0, Ordering::SeqCst);

    struct EmailOnly;
    impl Notifiable for EmailOnly {
        fn route_for(&self, channel: &str) -> Option<String> {
            if channel == "mail" {
                Some("only@example.org".into())
            } else {
                None
            }
        }
    }

    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(MailChannelStub))
        .register_channel(Arc::new(DbChannelStub));

    dispatcher
        .notify(
            &EmailOnly,
            &OrderShipped {
                tracking: "X".into(),
            },
        )
        .await
        .unwrap();

    assert_eq!(MAIL_HITS.load(Ordering::SeqCst), 1, "mail channel had a route");
    assert_eq!(
        DB_HITS.load(Ordering::SeqCst),
        0,
        "database channel had no route, must skip"
    );
}

#[tokio::test]
#[serial]
#[traced_test]
async fn notification_warns_when_declared_channel_is_unregistered() {
    MAIL_HITS.store(0, Ordering::SeqCst);
    DB_HITS.store(0, Ordering::SeqCst);

    // Only the mail channel is registered. OrderShipped declares both
    // "mail" and "database", so dispatch should warn about the missing
    // "database" channel and still deliver mail successfully.
    let dispatcher = NotificationDispatcher::new().register_channel(Arc::new(MailChannelStub));

    let recipient = Recipient {
        id: 7,
        email: "bob@example.org".into(),
    };
    let notification = OrderShipped {
        tracking: "WARN-1".into(),
    };

    dispatcher.notify(&recipient, &notification).await.unwrap();

    assert_eq!(MAIL_HITS.load(Ordering::SeqCst), 1, "mail still delivered");
    assert_eq!(DB_HITS.load(Ordering::SeqCst), 0, "no db channel registered");

    assert!(
        logs_contain("no channel registered"),
        "expected warn message about unregistered channel"
    );
    assert!(
        logs_contain("database"),
        "expected the missing channel name in the warn log"
    );
}

// Pin the documented "Returns on the first channel error; channels that
// already succeeded are not rolled back" contract on NotificationDispatcher::notify.
struct FailingMailChannel;
#[async_trait]
impl Channel for FailingMailChannel {
    fn name(&self) -> &'static str { "mail" }
    async fn deliver(
        &self,
        _route: &str,
        _notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal("simulated mail delivery failure"))
    }
}

#[tokio::test]
#[serial]
async fn notification_short_circuits_on_first_channel_error() {
    MAIL_HITS.store(0, Ordering::SeqCst);
    DB_HITS.store(0, Ordering::SeqCst);

    // OrderShipped declares channels in order ["mail", "database"]. With mail
    // failing first, the dispatcher must surface the mail error AND must not
    // invoke the database channel — that's the documented contract.
    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(FailingMailChannel))
        .register_channel(Arc::new(DbChannelStub));

    let recipient = Recipient {
        id: 100,
        email: "carol@example.org".into(),
    };
    let notification = OrderShipped {
        tracking: "FAIL-1".into(),
    };

    let err = dispatcher
        .notify(&recipient, &notification)
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("simulated mail delivery failure"),
        "first-channel error surfaces verbatim: {msg}"
    );
    assert_eq!(
        DB_HITS.load(Ordering::SeqCst),
        0,
        "subsequent channels must not run after first error"
    );
}

// Pin the documented "Last-write-wins" contract on register_channel.
struct AltMailChannelStub;
static ALT_MAIL_HITS: AtomicU32 = AtomicU32::new(0);
#[async_trait]
impl Channel for AltMailChannelStub {
    fn name(&self) -> &'static str { "mail" }
    async fn deliver(
        &self,
        _route: &str,
        _notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        ALT_MAIL_HITS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct MailOnlyNotification;
impl Notification for MailOnlyNotification {
    fn notification_name() -> &'static str { "MailOnlyNotification" }
    fn channels(&self) -> Vec<&'static str> { vec!["mail"] }
    fn data(&self) -> serde_json::Value { serde_json::json!({}) }
}

#[tokio::test]
#[serial]
async fn register_channel_is_last_write_wins() {
    MAIL_HITS.store(0, Ordering::SeqCst);
    ALT_MAIL_HITS.store(0, Ordering::SeqCst);

    // Two channels register under "mail"; the second must shadow the first.
    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(MailChannelStub))
        .register_channel(Arc::new(AltMailChannelStub));

    let recipient = Recipient {
        id: 1,
        email: "dan@example.org".into(),
    };
    dispatcher
        .notify(&recipient, &MailOnlyNotification)
        .await
        .unwrap();

    assert_eq!(
        MAIL_HITS.load(Ordering::SeqCst),
        0,
        "first registration must be shadowed"
    );
    assert_eq!(
        ALT_MAIL_HITS.load(Ordering::SeqCst),
        1,
        "second registration receives the delivery"
    );
}
