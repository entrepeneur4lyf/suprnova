//! Coverage for the `Notification::should_send` / `after_sending` hooks plus
//! the lifecycle events emitted by [`NotificationDispatcher::notify`].

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use suprnova::FrameworkError;
use suprnova::events::testing::{
    assert_dispatched as assert_event_dispatched, dispatched_count,
    install_fake as install_event_fake,
};
use suprnova::notifications::events::{NotificationFailed, NotificationSending, NotificationSent};
use suprnova::notifications::{
    Channel, DynNotification, Notifiable, Notification, NotificationDispatcher,
};

static MAIL_HITS: AtomicU32 = AtomicU32::new(0);
static AFTER_HITS: AtomicU32 = AtomicU32::new(0);

#[derive(Serialize, Deserialize, Debug, Clone)]
struct WelcomeNotification;

impl Notification for WelcomeNotification {
    fn notification_name() -> &'static str {
        "Welcome"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["mail", "database"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    fn after_sending(&self, _channel: &str) -> Result<(), FrameworkError> {
        AFTER_HITS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A notification that vetoes the mail channel via `should_send` but allows
/// the database channel through.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct VetoingNotification;

impl Notification for VetoingNotification {
    fn notification_name() -> &'static str {
        "Vetoing"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["mail", "database"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    fn should_send(&self, channel: &str) -> bool {
        channel != "mail"
    }
}

struct Recipient;

impl Notifiable for Recipient {
    fn route_for(&self, channel: &str) -> Option<String> {
        match channel {
            "mail" => Some("u@example.com".into()),
            "database" => Some("42".into()),
            _ => None,
        }
    }
}

struct StubMail;
#[async_trait]
impl Channel for StubMail {
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

struct FailingDb;
#[async_trait]
impl Channel for FailingDb {
    fn name(&self) -> &'static str {
        "database"
    }
    async fn deliver(
        &self,
        _route: &str,
        _notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal("synthetic db failure"))
    }
}

struct OkDb;
#[async_trait]
impl Channel for OkDb {
    fn name(&self) -> &'static str {
        "database"
    }
    async fn deliver(
        &self,
        _route: &str,
        _notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn should_send_false_skips_that_channel() {
    MAIL_HITS.store(0, Ordering::SeqCst);
    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(StubMail) as Arc<dyn Channel>)
        .register_channel(Arc::new(OkDb) as Arc<dyn Channel>);
    dispatcher
        .notify(&Recipient, &VetoingNotification)
        .await
        .unwrap();
    // mail vetoed → MAIL_HITS stays 0; database still delivered.
    assert_eq!(MAIL_HITS.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[serial]
async fn after_sending_runs_once_per_successful_channel() {
    AFTER_HITS.store(0, Ordering::SeqCst);
    MAIL_HITS.store(0, Ordering::SeqCst);
    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(StubMail) as Arc<dyn Channel>)
        .register_channel(Arc::new(OkDb) as Arc<dyn Channel>);
    dispatcher
        .notify(&Recipient, &WelcomeNotification)
        .await
        .unwrap();
    assert_eq!(MAIL_HITS.load(Ordering::SeqCst), 1);
    // mail + database both delivered → after_sending fires twice.
    assert_eq!(AFTER_HITS.load(Ordering::SeqCst), 2);
}

#[tokio::test]
#[serial]
async fn lifecycle_events_dispatched_around_each_channel() {
    let _guard = install_event_fake();
    MAIL_HITS.store(0, Ordering::SeqCst);
    AFTER_HITS.store(0, Ordering::SeqCst);

    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(StubMail) as Arc<dyn Channel>)
        .register_channel(Arc::new(OkDb) as Arc<dyn Channel>);
    dispatcher
        .notify(&Recipient, &WelcomeNotification)
        .await
        .unwrap();

    // 2 Sending + 2 Sent (one per channel) under the event fake.
    assert_eq!(dispatched_count::<NotificationSending>(|_| true), 2);
    assert_eq!(dispatched_count::<NotificationSent>(|_| true), 2);
    assert_eq!(dispatched_count::<NotificationFailed>(|_| true), 0);

    assert_event_dispatched::<NotificationSent>(|e| {
        e.notification == "Welcome" && e.channel == "mail" && e.route == "u@example.com"
    });
    assert_event_dispatched::<NotificationSent>(|e| {
        e.notification == "Welcome" && e.channel == "database" && e.route == "42"
    });
    assert_event_dispatched::<NotificationSending>(|e| {
        e.notification == "Welcome" && e.channel == "mail"
    });
}

#[tokio::test]
#[serial]
async fn failed_event_dispatched_when_channel_errors() {
    let _guard = install_event_fake();

    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(StubMail) as Arc<dyn Channel>)
        .register_channel(Arc::new(FailingDb) as Arc<dyn Channel>);
    let err = dispatcher
        .notify(&Recipient, &WelcomeNotification)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("synthetic db failure"));

    assert_eq!(dispatched_count::<NotificationFailed>(|_| true), 1);
    assert_event_dispatched::<NotificationFailed>(|e| {
        e.channel == "database" && e.error.contains("synthetic db failure")
    });
    // First channel (mail) succeeded before failure aborted the rest.
    assert_eq!(dispatched_count::<NotificationSent>(|_| true), 1);
    assert_event_dispatched::<NotificationSent>(|e| e.channel == "mail");
}
