//! Smoke test for `suprnova::prelude`.
//!
//! Pins the curated import surface: a typical mail-plus-notification
//! workflow must compile with only `use suprnova::prelude::*;` plus
//! third-party crates (serde, serial_test). Any prelude symbol removed
//! in the future surfaces here as a compile error rather than
//! downstream in a consumer.
//!
//! Marked `#[serial]` because the test mutates `Mail::TRANSPORT` and the
//! notification dispatcher / renderer registry globals.

use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::notifications::channels::mail::MailChannel;
use suprnova::notifications::{NotificationDispatcher, set_dispatcher};
use suprnova::prelude::*;
use suprnova::serde_json;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PreludeWelcome {
    name: String,
}

#[async_trait]
impl Mailable for PreludeWelcome {
    fn mailable_name() -> &'static str {
        "PreludeWelcome"
    }
    fn subject(&self) -> String {
        format!("Welcome aboard, {}", self.name)
    }
    fn text_template_source(&self) -> Option<String> {
        Some("Hi {{ name }}, welcome.".into())
    }
    fn from(&self) -> Option<Address> {
        Some("hello@suprnova.dev".into())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PreludeOrderShipped {
    tracking: String,
}

impl Notification for PreludeOrderShipped {
    fn notification_name() -> &'static str {
        "PreludeOrderShipped"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["mail"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "tracking": self.tracking })
    }
}

impl NotificationMailable for PreludeOrderShipped {
    fn to_mail(&self) -> Result<MailRendering, FrameworkError> {
        Ok(MailRendering {
            subject: format!("Order shipped — tracking {}", self.tracking),
            text: Some(format!("Tracking: {}", self.tracking)),
            ..Default::default()
        })
    }
}

struct PreludeCustomer {
    email: String,
}

impl Notifiable for PreludeCustomer {
    fn route_for(&self, channel: &str) -> Option<String> {
        match channel {
            "mail" => Some(self.email.clone()),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PreludePingJob;

#[async_trait]
impl Job for PreludePingJob {
    fn job_name() -> &'static str {
        "PreludePing"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn prelude_covers_typical_mail_and_notification_flow() {
    // Mail facade + Mailable + Address + MailFake — all from prelude.
    let fake = Mail::fake();
    Mail::to("alice@example.org")
        .send(PreludeWelcome {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    fake.assert_sent_count(1);
    fake.assert_sent(|m| m.subject == "Welcome aboard, Alice");

    // Notification facade + Notifiable + Notification + NotificationMailable
    // + MailRendering + register_mail_renderer — all from prelude.
    let _ = register_mail_renderer::<PreludeOrderShipped>();
    let dispatcher = NotificationDispatcher::new().register_channel(Arc::new(MailChannel::new()));
    let _ = set_dispatcher(Arc::new(dispatcher));

    Notify::send(
        &PreludeCustomer {
            email: "bob@example.org".into(),
        },
        &PreludeOrderShipped {
            tracking: "1Z-PRELUDE".into(),
        },
    )
    .await
    .unwrap();

    let captured = fake.captured();
    let last = captured.last().expect("notification produced a mail");
    assert_eq!(last.to[0].email, "bob@example.org");
    assert!(last.subject.contains("1Z-PRELUDE"));
}

/// Reachability pin — every symbol in the curated prelude must be
/// referenceable through `prelude::*` without further qualification.
/// If a re-export is dropped or renamed in `prelude.rs`, this stops
/// compiling.
#[test]
fn every_prelude_symbol_is_reachable() {
    fn _types_reachable() {
        let _: Option<FrameworkError> = None;
        let _: Option<AppError> = None;
        let _: Option<Box<dyn HttpError>> = None;
        let _: Option<Request> = None;
        let _: Option<Response> = None;
        let _: Option<HttpResponse> = None;
        let _: Option<Redirect> = None;
        let _: Option<Address> = None;
        let _: Option<Attachment> = None;
        let _: Option<MailFake> = None;
        let _: Option<MailRendering> = None;
    }
    // Facade types — pin them as ZST type references.
    fn _facades() {
        let _: Option<Mail> = None;
        let _: Option<Notify> = None;
        let _: Option<Bus> = None;
        let _: Option<Queue> = None;
        let _: Option<Cache> = None;
        let _: Option<Http> = None;
        let _: Option<Auth> = None;
        let _: Option<App> = None;
    }
    // Function pointers — proves the items are reachable.
    fn _functions() {
        let _: fn() = || {
            let _ = register_mail_renderer::<PreludeOrderShipped>();
        };
    }
    // Traits in scope — referenced via where-clauses.
    fn _traits<M, J, N, R>()
    where
        M: Mailable,
        J: Job,
        N: Notification,
        R: Notifiable,
    {
    }

    _types_reachable();
    _facades();
    _functions();
    _traits::<PreludeWelcome, PreludePingJob, PreludeOrderShipped, PreludeCustomer>();
}
