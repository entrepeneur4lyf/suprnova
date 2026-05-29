//! Laravel-13 parity integration tests for the Mail facade additions:
//! `Mail::raw` / `Mail::html` one-off sends, `always_from` / `always_to`
//! / `always_reply_to` / `always_return_path` globals, MailBuilder
//! fluent extensions (tag/metadata/priority/header/return_path/subject),
//! Mailable trait getters for the same hints, and the expanded
//! `MailFake` queued-side assertions (`assert_queued`,
//! `assert_nothing_queued`, `assert_queued_count`, `assert_outgoing_count`,
//! `assert_sent_to`, …).

use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use suprnova::async_trait;
use suprnova::mail::memory::InMemoryMailTransport;
use suprnova::mail::{
    Address, Mail, Mailable, OutgoingMessage, PRIORITY_HIGH, register_mailable_factory,
};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct WelcomeWithHints {
    name: String,
}

#[async_trait]
impl Mailable for WelcomeWithHints {
    fn mailable_name() -> &'static str {
        "ParityWelcome"
    }
    fn subject(&self) -> String {
        format!("Welcome, {}", self.name)
    }
    fn text_template_source(&self) -> Option<String> {
        Some("hi {{ name }}".into())
    }
    fn tags(&self) -> Vec<String> {
        vec!["onboarding".into()]
    }
    fn metadata(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("flow".into(), "signup".into());
        m
    }
    fn priority(&self) -> Option<u8> {
        Some(PRIORITY_HIGH)
    }
    fn headers(&self) -> Vec<(String, String)> {
        vec![("X-Mailable-Origin".into(), "Welcome".into())]
    }
}

#[tokio::test]
#[serial]
async fn mail_raw_sends_one_off_text_without_mailable() {
    let _ = Mail::forget_always();
    let fake = Mail::fake();

    Mail::raw("plain message", |b| {
        b.to("alice@example.org")
            .subject("hello")
            .from("ops@example.com")
    })
    .await
    .unwrap();

    fake.assert_sent_count(1);
    let captured = fake.captured();
    assert_eq!(captured[0].text.as_deref(), Some("plain message"));
    assert_eq!(captured[0].subject, "hello");
    assert!(captured[0].has_to("alice@example.org"));
    assert!(captured[0].has_from("ops@example.com"));
    assert!(captured[0].html.is_none());
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn mail_html_sends_one_off_html_without_mailable() {
    let _ = Mail::forget_always();
    let fake = Mail::fake();

    Mail::html("<p>hi</p>", |b| {
        b.to("alice@example.org")
            .subject("HTML-only")
            .from("ops@example.com")
    })
    .await
    .unwrap();

    fake.assert_sent_count(1);
    let captured = fake.captured();
    assert_eq!(captured[0].html.as_deref(), Some("<p>hi</p>"));
    assert!(captured[0].text.is_none());
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn always_from_applies_when_message_lacks_explicit_from() {
    let _ = Mail::forget_always();
    let _ = Mail::always_from(Address::new("ops@example.com").with_name("Operations"));
    let fake = Mail::fake();

    Mail::to("alice@example.org")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();

    let captured = fake.captured();
    // Mailable doesn't override `from`, so the global default applies.
    assert_eq!(captured[0].from.email, "ops@example.com");
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn always_to_overrides_recipients_and_clears_cc_bcc() {
    let _ = Mail::forget_always();
    let _ = Mail::always_to(Address::new("inbox@example.com"));
    let fake = Mail::fake();

    Mail::to("alice@example.org")
        .cc("manager@example.com")
        .bcc("audit@example.com")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();

    let captured = fake.captured();
    assert_eq!(captured[0].to.len(), 1);
    assert_eq!(captured[0].to[0].email, "inbox@example.com");
    assert!(captured[0].cc.is_empty());
    assert!(captured[0].bcc.is_empty());
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn always_reply_to_only_applies_when_message_has_no_reply_to() {
    let _ = Mail::forget_always();
    let _ = Mail::always_reply_to(Address::new("support@example.com"));
    let fake = Mail::fake();

    // No reply-to on message — default applies.
    Mail::to("alice@example.org")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    // Explicit reply_to — default does NOT override.
    Mail::to("alice@example.org")
        .reply_to("custom@example.com")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();

    let captured = fake.captured();
    assert_eq!(captured[0].reply_to.len(), 1);
    assert_eq!(captured[0].reply_to[0].email, "support@example.com");
    assert_eq!(captured[1].reply_to.len(), 1);
    assert_eq!(captured[1].reply_to[0].email, "custom@example.com");
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn always_return_path_applies_when_unset() {
    let _ = Mail::forget_always();
    let _ = Mail::always_return_path(Address::new("bounce@example.com"));
    let fake = Mail::fake();

    Mail::to("alice@example.org")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();

    let captured = fake.captured();
    assert_eq!(
        captured[0].return_path.as_ref().map(|a| a.email.as_str()),
        Some("bounce@example.com")
    );
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn mailable_hints_forward_to_outgoing_message() {
    let _ = Mail::forget_always();
    let fake = Mail::fake();
    Mail::to("alice@example.org")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    let captured = fake.captured();
    assert!(captured[0].has_tag("onboarding"));
    assert!(captured[0].metadata_equals("flow", "signup"));
    assert_eq!(captured[0].priority, Some(PRIORITY_HIGH));
    assert!(captured[0].has_header("X-Mailable-Origin", "Welcome"));
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn builder_hints_merge_with_mailable_hints() {
    let _ = Mail::forget_always();
    let fake = Mail::fake();
    Mail::to("alice@example.org")
        .tag("campaign-spring")
        .metadata("flow", "trial") // overrides mailable's flow=signup
        .header("X-Source", "promo")
        .priority(1) // overrides mailable PRIORITY_HIGH
        .return_path("bounce@example.com")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    let captured = fake.captured();
    // Both tags present (mailable + builder, de-duped).
    assert!(captured[0].has_tag("onboarding"));
    assert!(captured[0].has_tag("campaign-spring"));
    // Builder overrides metadata key collision.
    assert!(captured[0].metadata_equals("flow", "trial"));
    // Builder priority wins.
    assert_eq!(captured[0].priority, Some(1));
    // Both header lines (mailable's + builder's) coexist.
    assert!(captured[0].has_header("X-Mailable-Origin", "Welcome"));
    assert!(captured[0].has_header("X-Source", "promo"));
    // Return path layered by builder.
    assert_eq!(
        captured[0].return_path.as_ref().map(|a| a.email.as_str()),
        Some("bounce@example.com")
    );
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn builder_subject_override_wins_over_mailable_render_subject() {
    let _ = Mail::forget_always();
    let fake = Mail::fake();
    Mail::to("alice@example.org")
        .subject("Override Subject")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    let captured = fake.captured();
    assert_eq!(captured[0].subject, "Override Subject");
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn fake_assert_queued_finds_mailables_pushed_through_builder_queue() {
    let _ = Mail::forget_always();
    let _ = register_mailable_factory::<WelcomeWithHints>();
    let fake = Mail::fake();

    Mail::to("alice@example.org")
        .queue(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    Mail::to("bob@example.org")
        .queue(WelcomeWithHints { name: "Bob".into() })
        .await
        .unwrap();

    // Nothing went through the sent transport — queued capture only.
    fake.assert_nothing_sent();
    fake.assert_queued("ParityWelcome");
    fake.assert_queued_count(2);
    fake.assert_queued_to("alice@example.org");
    fake.assert_queued_to("bob@example.org");
    fake.assert_outgoing_count(2);
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn fake_assert_queued_with_decodes_concrete_mailable() {
    let _ = Mail::forget_always();
    let _ = register_mailable_factory::<WelcomeWithHints>();
    let fake = Mail::fake();

    Mail::to("alice@example.org")
        .queue(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();

    fake.assert_queued_with("ParityWelcome", |q| {
        let decoded: WelcomeWithHints = q.decode().expect("decode");
        decoded.name == "Alice" && q.has_to("alice@example.org")
    });
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn fake_assert_not_queued_and_nothing_queued() {
    let _ = Mail::forget_always();
    let _ = register_mailable_factory::<WelcomeWithHints>();
    let fake = Mail::fake();
    fake.assert_nothing_queued();
    fake.assert_not_queued("ParityWelcome");
    fake.assert_outgoing_count(0);
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn fake_later_captures_delay_on_snapshot() {
    let _ = Mail::forget_always();
    let _ = register_mailable_factory::<WelcomeWithHints>();
    let fake = Mail::fake();

    Mail::to("alice@example.org")
        .later(
            Duration::from_secs(60),
            WelcomeWithHints {
                name: "Alice".into(),
            },
        )
        .await
        .unwrap();

    let queued = fake.queued();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].delay, Some(Duration::from_secs(60)));
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn fake_sent_to_filters_messages_by_recipient() {
    let _ = Mail::forget_always();
    let fake = Mail::fake();
    Mail::to("alice@example.org")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    Mail::to("bob@example.org")
        .send(WelcomeWithHints { name: "Bob".into() })
        .await
        .unwrap();

    let alice = fake.sent_to("alice@example.org");
    assert_eq!(alice.len(), 1);
    assert!(alice[0].has_to("alice@example.org"));
    fake.assert_sent_to("alice@example.org");
    fake.assert_not_sent_to("eve@example.com");
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn outgoing_message_inspection_helpers() {
    let mut msg = OutgoingMessage::new(Address::new("ops@example.com"));
    msg.to = vec![Address::new("alice@example.org")];
    msg.cc = vec![Address::new("manager@example.com")];
    msg.reply_to = vec![Address::new("support@example.com")];
    msg.subject = "hello".into();
    msg.tags = vec!["welcome".into()];
    msg.metadata.insert("k".into(), "v".into());

    assert!(msg.has_to("ALICE@example.org")); // case-insensitive
    assert!(msg.has_cc("manager@example.com"));
    assert!(!msg.has_bcc("nope@example.com"));
    assert!(msg.has_reply_to("support@example.com"));
    assert!(msg.has_from("ops@example.com"));
    assert!(msg.has_subject("hello"));
    assert!(msg.has_tag("welcome"));
    assert!(msg.has_metadata("k"));
    assert!(msg.metadata_equals("k", "v"));
    assert!(!msg.metadata_equals("k", "wrong"));
}

#[tokio::test]
#[serial]
async fn always_from_returns_previous_value_for_restore() {
    let _ = Mail::forget_always();
    assert_eq!(
        Mail::always_from(Address::new("a@example.com")).unwrap(),
        None
    );
    let prev = Mail::always_from(Address::new("b@example.com")).unwrap();
    assert_eq!(
        prev.as_ref().map(|a| a.email.as_str()),
        Some("a@example.com")
    );
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn fake_drop_clears_queued_capture_for_sibling_tests() {
    let _ = Mail::forget_always();
    let _ = register_mailable_factory::<WelcomeWithHints>();
    {
        let fake = Mail::fake();
        Mail::to("alice@example.org")
            .queue(WelcomeWithHints {
                name: "Alice".into(),
            })
            .await
            .unwrap();
        fake.assert_queued_count(1);
    } // fake drops here — clears queue capture
    let fake = Mail::fake();
    fake.assert_queued_count(0);
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn fake_send_attaches_builder_attachment() {
    use suprnova::mail::Attachment;
    let _ = Mail::forget_always();
    let fake = Mail::fake();
    Mail::to("alice@example.org")
        .attach(Attachment::new("note.txt", b"hi".to_vec(), "text/plain"))
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    let captured = fake.captured();
    assert!(captured[0].has_attachment("note.txt"));
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
async fn fake_restores_existing_transport_on_drop_with_queued_capture_cleared() {
    let _ = Mail::forget_always();
    let prior = Arc::new(InMemoryMailTransport::new());
    let _ = Mail::set_transport(prior.clone());

    {
        let fake = Mail::fake();
        let _ = register_mailable_factory::<WelcomeWithHints>();
        Mail::to("alice@example.org")
            .queue(WelcomeWithHints {
                name: "Alice".into(),
            })
            .await
            .unwrap();
        fake.assert_queued_count(1);
        // fake drops here
    }

    // After drop: prior is back, queue capture is cleared.
    let fake = Mail::fake();
    fake.assert_queued_count(0);
    drop(fake);
    let _ = Mail::clear_transport();
    let _ = Mail::forget_always();
}

#[tokio::test]
#[serial]
#[should_panic(expected = "expected at least one queued ParityWelcome")]
async fn fake_assert_queued_panics_when_none_match() {
    let _ = Mail::forget_always();
    let _ = register_mailable_factory::<WelcomeWithHints>();
    let fake = Mail::fake();
    fake.assert_queued("ParityWelcome");
}

#[tokio::test]
#[serial]
#[should_panic(expected = "expected NO queued ParityWelcome")]
async fn fake_assert_not_queued_panics_when_one_matches() {
    let _ = Mail::forget_always();
    let _ = register_mailable_factory::<WelcomeWithHints>();
    let fake = Mail::fake();
    Mail::to("alice@example.org")
        .queue(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    fake.assert_not_queued("ParityWelcome");
}

#[tokio::test]
#[serial]
#[should_panic(expected = "expected NO messages sent")]
async fn fake_assert_nothing_sent_panics_when_one_was_sent() {
    let _ = Mail::forget_always();
    let fake = Mail::fake();
    Mail::to("alice@example.org")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    fake.assert_nothing_sent();
}

#[tokio::test]
#[serial]
async fn mail_cc_and_bcc_entry_points_start_builder() {
    let _ = Mail::forget_always();
    let fake = Mail::fake();
    Mail::cc("manager@example.com")
        .to("alice@example.org")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    Mail::bcc("audit@example.com")
        .to("alice@example.org")
        .send(WelcomeWithHints {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    let captured = fake.captured();
    assert!(captured[0].has_cc("manager@example.com"));
    assert!(captured[1].has_bcc("audit@example.com"));
    let _ = Mail::forget_always();
}
