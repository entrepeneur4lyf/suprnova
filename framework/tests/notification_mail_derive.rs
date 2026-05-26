//! Coverage for `#[derive(NotificationMailable)]`.
//!
//! Pins the attribute-driven `to_mail` generator across every branch
//! the macro supports: inline templates, file-backed templates via
//! `include_str!`, sender + display name, comma-separated cc/bcc/
//! reply_to lists, and text-only renderings.
//!
//! End-to-end branch: the derive flows through `register_mail_renderer`
//! → `MailChannel::deliver` → `dispatch_with_telemetry` → the
//! `InMemoryMailTransport` capture buffer, so we assert against the
//! actual `OutgoingMessage` to prove the wiring is correct, not just
//! that `to_mail` builds a struct.
//!
//! All tests are `#[serial]` — they touch the renderer registry, the
//! dispatcher, and the mail transport, all of which are process-global.

use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::NotificationMailable;
use suprnova::mail::Mail;
use suprnova::notifications::channels::mail::{MailChannel, register_mail_renderer};
use suprnova::notifications::{
    Notifiable, Notification, NotificationDispatcher, Notify, set_dispatcher,
};
use suprnova::serde_json;

struct Recipient {
    email: String,
}
impl Notifiable for Recipient {
    fn route_for(&self, channel: &str) -> Option<String> {
        match channel {
            "mail" => Some(self.email.clone()),
            _ => None,
        }
    }
}

fn channels_mail() -> Vec<&'static str> {
    vec!["mail"]
}

// ============================================================================
// Variant 1 — inline html + text, no sender override
// ============================================================================

#[derive(Serialize, Deserialize, NotificationMailable)]
#[mail(
    subject = "Hello {{ name }}",
    html = "<p>Hi {{ name }}!</p>",
    text = "Hi {{ name }}!"
)]
struct InlineBoth {
    name: String,
}
impl Notification for InlineBoth {
    fn notification_name() -> &'static str {
        "InlineBoth"
    }
    fn channels(&self) -> Vec<&'static str> {
        channels_mail()
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "name": self.name })
    }
}

#[tokio::test]
#[serial]
async fn derive_inline_html_and_text() {
    let fake = Mail::fake();
    register_mail_renderer::<InlineBoth>();
    set_dispatcher(Arc::new(
        NotificationDispatcher::new().register_channel(Arc::new(MailChannel::new())),
    ));

    Notify::send(
        &Recipient {
            email: "alice@example.org".into(),
        },
        &InlineBoth {
            name: "Alice".into(),
        },
    )
    .await
    .unwrap();

    let msgs = fake.captured();
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];
    assert_eq!(m.subject, "Hello Alice");
    assert_eq!(m.html.as_deref(), Some("<p>Hi Alice!</p>"));
    assert_eq!(m.text.as_deref(), Some("Hi Alice!"));
    assert_eq!(
        m.from.email, "noreply@localhost",
        "no `from` attribute → channel falls back to noreply"
    );
    assert!(m.cc.is_empty());
    assert!(m.bcc.is_empty());
    assert!(m.reply_to.is_empty());
}

// ============================================================================
// Variant 2 — text only (no html); from + from_name override
// ============================================================================

#[derive(Serialize, Deserialize, NotificationMailable)]
#[mail(
    subject = "Order #{{ order_id }} text-only",
    text = "Your order {{ order_id }} is queued.",
    from = "orders@suprnova.dev",
    from_name = "Suprnova Orders"
)]
struct TextOnlyOrder {
    order_id: u64,
}
impl Notification for TextOnlyOrder {
    fn notification_name() -> &'static str {
        "TextOnlyOrder"
    }
    fn channels(&self) -> Vec<&'static str> {
        channels_mail()
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "order_id": self.order_id })
    }
}

#[tokio::test]
#[serial]
async fn derive_text_only_with_from_and_from_name() {
    let fake = Mail::fake();
    register_mail_renderer::<TextOnlyOrder>();
    set_dispatcher(Arc::new(
        NotificationDispatcher::new().register_channel(Arc::new(MailChannel::new())),
    ));

    Notify::send(
        &Recipient {
            email: "bob@example.org".into(),
        },
        &TextOnlyOrder { order_id: 42 },
    )
    .await
    .unwrap();

    let msgs = fake.captured();
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];
    assert_eq!(m.subject, "Order #42 text-only");
    assert!(m.html.is_none(), "no html configured");
    assert_eq!(m.text.as_deref(), Some("Your order 42 is queued."));
    assert_eq!(m.from.email, "orders@suprnova.dev");
    assert_eq!(m.from.name.as_deref(), Some("Suprnova Orders"));
}

// ============================================================================
// Variant 3 — cc / bcc / reply_to comma-separated lists
// ============================================================================

#[derive(Serialize, Deserialize, NotificationMailable)]
#[mail(
    subject = "Audit {{ event }}",
    text = "Event: {{ event }}",
    from = "audit@suprnova.dev",
    cc = "team@suprnova.dev, ops@suprnova.dev",
    bcc = "trail@suprnova.dev",
    reply_to = "support@suprnova.dev"
)]
struct AuditNotice {
    event: String,
}
impl Notification for AuditNotice {
    fn notification_name() -> &'static str {
        "AuditNotice"
    }
    fn channels(&self) -> Vec<&'static str> {
        channels_mail()
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "event": self.event })
    }
}

#[tokio::test]
#[serial]
async fn derive_cc_bcc_reply_to_lists_thread_through() {
    let fake = Mail::fake();
    register_mail_renderer::<AuditNotice>();
    set_dispatcher(Arc::new(
        NotificationDispatcher::new().register_channel(Arc::new(MailChannel::new())),
    ));

    Notify::send(
        &Recipient {
            email: "ops@example.org".into(),
        },
        &AuditNotice {
            event: "policy-changed".into(),
        },
    )
    .await
    .unwrap();

    let msgs = fake.captured();
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];

    let emails = |xs: &[suprnova::mail::Address]| -> Vec<String> {
        xs.iter().map(|a| a.email.clone()).collect()
    };
    assert_eq!(emails(&m.cc), vec!["team@suprnova.dev", "ops@suprnova.dev"]);
    assert_eq!(emails(&m.bcc), vec!["trail@suprnova.dev"]);
    assert_eq!(emails(&m.reply_to), vec!["support@suprnova.dev"]);
    assert!(m.subject.contains("policy-changed"));
}

// ============================================================================
// Variant 4 — file templates via `include_str!`
// ============================================================================

#[derive(Serialize, Deserialize, NotificationMailable)]
#[mail(
    subject = "Welcome {{ name }}",
    html_template = "templates/derive_test.html",
    text_template = "templates/derive_test.txt",
    from = "welcome@suprnova.dev"
)]
struct FileTemplated {
    name: String,
    token: String,
}
impl Notification for FileTemplated {
    fn notification_name() -> &'static str {
        "FileTemplated"
    }
    fn channels(&self) -> Vec<&'static str> {
        channels_mail()
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "name": self.name, "token": self.token })
    }
}

#[tokio::test]
#[serial]
async fn derive_html_and_text_templates_via_include_str() {
    let fake = Mail::fake();
    register_mail_renderer::<FileTemplated>();
    set_dispatcher(Arc::new(
        NotificationDispatcher::new().register_channel(Arc::new(MailChannel::new())),
    ));

    Notify::send(
        &Recipient {
            email: "carol@example.org".into(),
        },
        &FileTemplated {
            name: "Carol".into(),
            token: "ABCXYZ".into(),
        },
    )
    .await
    .unwrap();

    let msgs = fake.captured();
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];
    assert_eq!(m.subject, "Welcome Carol");
    let html = m.html.as_deref().expect("html present from include_str");
    assert!(
        html.contains("Hello Carol"),
        "html rendered with name: {html}"
    );
    assert!(html.contains("ABCXYZ"), "html rendered with token: {html}");
    let text = m.text.as_deref().expect("text present from include_str");
    assert!(text.contains("Hello Carol"));
    assert!(text.contains("ABCXYZ"));
}

// ============================================================================
// Variant 5 — html only (no text)
// ============================================================================

#[derive(Serialize, Deserialize, NotificationMailable)]
#[mail(subject = "HTML-only {{ note }}", html = "<h1>{{ note }}</h1>")]
struct HtmlOnly {
    note: String,
}
impl Notification for HtmlOnly {
    fn notification_name() -> &'static str {
        "HtmlOnly"
    }
    fn channels(&self) -> Vec<&'static str> {
        channels_mail()
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "note": self.note })
    }
}

#[tokio::test]
#[serial]
async fn derive_html_only_leaves_text_none() {
    let fake = Mail::fake();
    register_mail_renderer::<HtmlOnly>();
    set_dispatcher(Arc::new(
        NotificationDispatcher::new().register_channel(Arc::new(MailChannel::new())),
    ));

    Notify::send(
        &Recipient {
            email: "dave@example.org".into(),
        },
        &HtmlOnly {
            note: "hello".into(),
        },
    )
    .await
    .unwrap();

    let msgs = fake.captured();
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];
    assert_eq!(m.html.as_deref(), Some("<h1>hello</h1>"));
    assert!(m.text.is_none(), "no text variant configured");
}

// ============================================================================
// Variant 6 — trailing comma / extra whitespace in address lists
// ============================================================================

#[derive(Serialize, Deserialize, NotificationMailable)]
#[mail(
    subject = "Trim test",
    text = "ok",
    cc = "  one@x.com  ,,two@x.com,   ,"
)]
struct TrailingWhitespace {
    // A placeholder field keeps Tera's context happy (it expects a
    // JSON object) and gives serde a real shape to round-trip through
    // the renderer registry. Notifications with zero variation are
    // rare in practice; this is documentation-by-test for "use a
    // fielded struct."
    _placeholder: (),
}
impl Notification for TrailingWhitespace {
    fn notification_name() -> &'static str {
        "TrailingWhitespace"
    }
    fn channels(&self) -> Vec<&'static str> {
        channels_mail()
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "_placeholder": null })
    }
}

#[tokio::test]
#[serial]
async fn derive_address_list_trims_whitespace_and_skips_empties() {
    let fake = Mail::fake();
    register_mail_renderer::<TrailingWhitespace>();
    set_dispatcher(Arc::new(
        NotificationDispatcher::new().register_channel(Arc::new(MailChannel::new())),
    ));

    Notify::send(
        &Recipient {
            email: "any@example.org".into(),
        },
        &TrailingWhitespace { _placeholder: () },
    )
    .await
    .unwrap();

    let msgs = fake.captured();
    assert_eq!(msgs.len(), 1);
    let cc_emails: Vec<String> = msgs[0].cc.iter().map(|a| a.email.clone()).collect();
    assert_eq!(
        cc_emails,
        vec!["one@x.com", "two@x.com"],
        "whitespace trimmed; empty entries dropped"
    );
}
