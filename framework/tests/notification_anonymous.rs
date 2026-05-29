//! Coverage for `AnonymousNotifiable` + `Notify::route` / `Notify::routes`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use suprnova::FrameworkError;
use suprnova::notifications::{
    AnonymousNotifiable, Channel, DynNotification, Notifiable, Notification,
    NotificationDispatcher, Notify, set_dispatcher,
};

static MAIL_HITS: AtomicU32 = AtomicU32::new(0);

#[derive(Serialize, Deserialize, Debug, Clone)]
struct IncidentAlert {
    code: String,
}

impl Notification for IncidentAlert {
    fn notification_name() -> &'static str {
        "IncidentAlert"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["mail", "slack"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "code": self.code })
    }
}

struct CapturingMail;

#[async_trait]
impl Channel for CapturingMail {
    fn name(&self) -> &'static str {
        "mail"
    }
    async fn deliver(
        &self,
        route: &str,
        notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        assert_eq!(notification.name(), "IncidentAlert");
        assert_eq!(route, "ops@example.com");
        MAIL_HITS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn route_for_returns_configured_routes() {
    let anon = AnonymousNotifiable::new()
        .route("mail", "ops@example.com")
        .unwrap()
        .route("slack", "#alerts")
        .unwrap();
    assert_eq!(anon.route_for("mail"), Some("ops@example.com".to_string()));
    assert_eq!(anon.route_for("slack"), Some("#alerts".to_string()));
    assert_eq!(anon.route_for("vonage"), None);
}

#[test]
fn database_channel_is_rejected_on_anonymous() {
    let err = AnonymousNotifiable::new()
        .route("database", "42")
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("database channel does not support on-demand"),
        "expected database rejection, got: {msg}"
    );
}

#[test]
fn routes_iter_threads_through_route_validation() {
    let anon = AnonymousNotifiable::new()
        .routes([("mail", "ops@example.com"), ("slack", "#alerts")])
        .unwrap();
    assert_eq!(anon.route_for("mail"), Some("ops@example.com".to_string()));
    // database-channel rejection still fires when used inside a batch.
    let err = AnonymousNotifiable::new()
        .routes([("mail", "ok@example.com"), ("database", "42")])
        .unwrap_err();
    assert!(format!("{err}").contains("database channel"));
}

#[test]
fn notify_route_and_routes_are_facade_helpers() {
    let one = Notify::route("mail", "ops@example.com").unwrap();
    assert_eq!(one.route_for("mail"), Some("ops@example.com".to_string()));

    let many = Notify::routes([("mail", "ops@example.com"), ("slack", "#alerts")]).unwrap();
    assert_eq!(many.route_for("slack"), Some("#alerts".to_string()));
    assert!(Notify::route("database", "42").is_err());
}

#[tokio::test]
#[serial]
async fn notify_send_against_anonymous_recipient() {
    MAIL_HITS.store(0, Ordering::SeqCst);
    let dispatcher = Arc::new(
        NotificationDispatcher::new().register_channel(Arc::new(CapturingMail) as Arc<dyn Channel>),
    );
    set_dispatcher(dispatcher).unwrap();

    let recipient = Notify::route("mail", "ops@example.com").unwrap();
    Notify::send(&recipient, &IncidentAlert { code: "E-7".into() })
        .await
        .unwrap();

    // Slack route absent → that channel is silently skipped, matching the
    // dispatcher contract for any Notifiable.
    assert_eq!(MAIL_HITS.load(Ordering::SeqCst), 1);
}
