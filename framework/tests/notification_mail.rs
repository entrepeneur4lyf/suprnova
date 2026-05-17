use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::mail::memory::InMemoryMailTransport;
use suprnova::mail::{Address, Mail};
use suprnova::notifications::channels::mail::{MailChannel, MailRendering};
use suprnova::notifications::{Notifiable, Notification, NotificationDispatcher};
use suprnova::FrameworkError;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct OrderShipped {
    tracking: String,
}

impl Notification for OrderShipped {
    fn notification_name() -> &'static str {
        "OrderShipped"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["mail"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "tracking": self.tracking })
    }
}

struct User {
    email: String,
}

impl Notifiable for User {
    fn route_for(&self, channel: &str) -> Option<String> {
        if channel == "mail" {
            Some(self.email.clone())
        } else {
            None
        }
    }
}

#[tokio::test]
#[serial]
async fn mail_channel_dispatches_through_bound_transport() {
    // Capture our own Arc to the transport before binding it globally so
    // we can read the captured-message buffer post-dispatch without racing
    // against the next test's `set_transport`.
    let transport = Arc::new(InMemoryMailTransport::new());
    Mail::set_transport(transport.clone());

    let channel = MailChannel::new(|name, data| {
        assert_eq!(name, "OrderShipped");
        let tracking = data["tracking"].as_str().unwrap_or_default().to_string();
        Ok(MailRendering {
            subject: format!("Your order shipped ({tracking})"),
            html: None,
            text: Some(format!("Tracking number: {tracking}")),
            from: Some(Address::new("orders@suprnova.dev").with_name("Suprnova Orders")),
        })
    });

    let dispatcher = NotificationDispatcher::new().register_channel(Arc::new(channel));

    let recipient = User {
        email: "alice@example.org".into(),
    };
    dispatcher
        .notify(
            &recipient,
            &OrderShipped {
                tracking: "1Z999".into(),
            },
        )
        .await
        .unwrap();

    let captured = transport.captured();
    assert_eq!(captured.len(), 1, "exactly one message captured");
    let msg = &captured[0];
    assert_eq!(msg.from.email, "orders@suprnova.dev");
    assert_eq!(msg.from.name.as_deref(), Some("Suprnova Orders"));
    assert_eq!(msg.to.len(), 1);
    assert_eq!(msg.to[0].email, "alice@example.org");
    assert_eq!(msg.subject, "Your order shipped (1Z999)");
    assert_eq!(msg.text.as_deref(), Some("Tracking number: 1Z999"));
    assert!(msg.html.is_none(), "html intentionally absent");
}

#[tokio::test]
#[serial]
async fn mail_channel_empty_body_fails_fast() {
    // Bind a transport so any error must come from the empty-body guard,
    // not from a missing transport. Without this binding a future
    // reorder of the guard / transport-lookup could silently change
    // which error surfaces.
    let transport = Arc::new(InMemoryMailTransport::new());
    Mail::set_transport(transport.clone());

    let channel = MailChannel::new(|_name, _data| {
        Ok(MailRendering {
            subject: "ignored".into(),
            html: None,
            text: None,
            from: None,
        })
    });
    let dispatcher = NotificationDispatcher::new().register_channel(Arc::new(channel));

    let err = dispatcher
        .notify(
            &User {
                email: "bob@example.org".into(),
            },
            &OrderShipped {
                tracking: "X".into(),
            },
        )
        .await
        .unwrap_err();

    let msg = format!("{err}");
    assert!(
        msg.contains("MailChannel"),
        "expected MailChannel error context, got: {msg}"
    );
    assert!(
        msg.contains("OrderShipped"),
        "expected notification name in error, got: {msg}"
    );
    assert!(
        msg.contains("no html or text body"),
        "expected guard message, got: {msg}"
    );
    assert!(
        transport.captured().is_empty(),
        "guard must fire before transport.send is invoked"
    );
}

#[tokio::test]
#[serial]
async fn mail_channel_propagates_factory_error() {
    // Same precaution as above — bind a transport so the test pins
    // factory-error propagation rather than masking it with a
    // missing-transport error.
    Mail::set_transport(Arc::new(InMemoryMailTransport::new()));

    let channel = MailChannel::new(|_name, _data| {
        Err(FrameworkError::internal("factory boom"))
    });
    let dispatcher = NotificationDispatcher::new().register_channel(Arc::new(channel));

    let err = dispatcher
        .notify(
            &User {
                email: "carol@example.org".into(),
            },
            &OrderShipped {
                tracking: "Y".into(),
            },
        )
        .await
        .unwrap_err();

    let msg = format!("{err}");
    assert!(
        msg.contains("factory boom"),
        "expected factory error to surface verbatim, got: {msg}"
    );
}
