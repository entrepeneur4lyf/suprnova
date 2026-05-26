//! Telemetry pin for the `notification.dispatch` span emitted by
//! [`NotificationDispatcher::notify`] (and therefore `Notify::send`
//! and the `SendNotificationJob` queue worker).
//!
//! Wraps the channel fan-out; inside the span, per-channel sends emit
//! their own events (e.g. `mail.send`). Pins both presence + the
//! `duration_ms` field on the completion event.
//!
//! All tests are `#[serial]` — `Mail::TRANSPORT`, the dispatcher
//! global, and the mail renderer registry are all process-global.

use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::FrameworkError;
use suprnova::mail::Mail;
use suprnova::notifications::channels::mail::{
    MailChannel, MailRendering, NotificationMailable, register_mail_renderer,
};
use suprnova::notifications::{
    Notifiable, Notification, NotificationDispatcher, Notify, set_dispatcher,
};
use suprnova::serde_json;
use tracing_test::traced_test;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct OrderReady {
    order_id: u64,
}

impl Notification for OrderReady {
    fn notification_name() -> &'static str {
        "OrderReady"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["mail"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "order_id": self.order_id })
    }
}

impl NotificationMailable for OrderReady {
    fn to_mail(&self) -> Result<MailRendering, FrameworkError> {
        Ok(MailRendering {
            subject: format!("Order #{} ready", self.order_id),
            text: Some(format!("Your order #{} is ready.", self.order_id)),
            ..Default::default()
        })
    }
}

struct Customer {
    email: String,
}

impl Notifiable for Customer {
    fn route_for(&self, channel: &str) -> Option<String> {
        match channel {
            "mail" => Some(self.email.clone()),
            _ => None,
        }
    }
}

#[tokio::test]
#[traced_test]
#[serial]
async fn notify_send_emits_dispatch_span_wrapping_mail_send() {
    let _fake = Mail::fake();
    let _ = register_mail_renderer::<OrderReady>();

    let dispatcher = NotificationDispatcher::new().register_channel(Arc::new(MailChannel::new()));
    let _ = set_dispatcher(Arc::new(dispatcher));

    Notify::send(
        &Customer {
            email: "ada@example.org".into(),
        },
        &OrderReady { order_id: 7 },
    )
    .await
    .unwrap();

    // Outer span over the fan-out.
    assert!(
        logs_contain("notification.dispatch"),
        "dispatch span must be present"
    );
    assert!(
        logs_contain("notification=\"OrderReady\""),
        "notification name on the dispatch span"
    );
    assert!(
        logs_contain("channel_count=1"),
        "channel_count on the dispatch span"
    );
    assert!(
        logs_contain("notification dispatched"),
        "success completion event for dispatch"
    );

    // Inner span emitted by the mail channel's send.
    assert!(
        logs_contain("mail.send"),
        "mail.send span emitted inside the dispatch span"
    );
    assert!(logs_contain("mail sent"), "mail success event present");
    assert!(
        logs_contain("duration_ms="),
        "duration_ms recorded on completion events"
    );
}

#[tokio::test]
#[traced_test]
#[serial]
async fn notify_send_unregistered_channel_warn_lives_under_dispatch_span() {
    // Skip the renderer registration so the mail channel WOULD fail, but
    // also skip registering the channel — so the dispatcher just warns
    // and the dispatch span still completes successfully.
    let _fake = Mail::fake();

    let dispatcher = NotificationDispatcher::new(); // no channels
    let _ = set_dispatcher(Arc::new(dispatcher));

    Notify::send(
        &Customer {
            email: "ada@example.org".into(),
        },
        &OrderReady { order_id: 9 },
    )
    .await
    .unwrap();

    assert!(
        logs_contain("notification.dispatch"),
        "dispatch span still present"
    );
    assert!(
        logs_contain("no channel registered"),
        "warn message for missing channel"
    );
    assert!(
        logs_contain("notification dispatched"),
        "dispatch completes successfully when all channels are missing"
    );
}
