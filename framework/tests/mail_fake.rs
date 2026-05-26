//! `Mail::fake()` facade integration tests.
//!
//! Mirrors the `Bus::fake` / `Queue::fake` / `Cache::fake` patterns
//! established in Phase 5A. The fake guard captures every dispatched
//! message in memory; on drop, the previously-bound transport (or
//! its absence) is restored. Every test marks itself `#[serial]`
//! because the underlying `TRANSPORT` static is process-global.

use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::mail::memory::InMemoryMailTransport;
use suprnova::mail::{Address, Mail, Mailable};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Greeting {
    name: String,
}

#[async_trait]
impl Mailable for Greeting {
    fn mailable_name() -> &'static str {
        "Greeting"
    }
    fn subject(&self) -> String {
        format!("Hello, {}", self.name)
    }
    fn text_template_source(&self) -> Option<String> {
        Some("Welcome aboard, {{ name }}.".into())
    }
    fn from(&self) -> Option<Address> {
        Some("hello@suprnova.dev".into())
    }
}

#[tokio::test]
#[serial]
async fn fake_captures_dispatched_messages_and_supports_basic_assertions() {
    let fake = Mail::fake();

    Mail::to("alice@example.org")
        .send(Greeting {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    Mail::to("bob@example.org")
        .send(Greeting { name: "Bob".into() })
        .await
        .unwrap();

    fake.assert_sent_count(2);
    fake.assert_sent(|m| m.to.iter().any(|a| a.email == "alice@example.org"));
    fake.assert_sent(|m| m.subject == "Hello, Bob");
    fake.assert_not_sent(|m| m.to.iter().any(|a| a.email == "eve@example.org"));

    let captured = fake.captured();
    assert_eq!(captured.len(), 2, "captured() returns the full set");
    assert_eq!(fake.count(), 2, "count() matches captured().len()");
}

#[tokio::test]
#[serial]
async fn fake_restores_previously_bound_transport_on_drop() {
    // Bind a distinct transport BEFORE faking so we can confirm
    // restoration after drop.
    let prior = Arc::new(InMemoryMailTransport::new());
    Mail::set_transport(prior.clone());

    // Send through the prior transport so we can later verify it's
    // still the one in scope after the fake drops.
    Mail::to("prior@example.org")
        .send(Greeting {
            name: "Prior".into(),
        })
        .await
        .unwrap();
    assert_eq!(prior.captured().len(), 1, "prior transport recorded one");

    {
        let fake = Mail::fake();
        // While the fake is live, prior is shadowed.
        Mail::to("during@example.org")
            .send(Greeting {
                name: "During".into(),
            })
            .await
            .unwrap();
        fake.assert_sent_count(1);
        assert_eq!(
            prior.captured().len(),
            1,
            "prior must not see messages while fake is bound"
        );
        // `fake` drops here.
    }

    // Prior transport is back — a fresh dispatch lands on it.
    Mail::to("after@example.org")
        .send(Greeting {
            name: "After".into(),
        })
        .await
        .unwrap();
    let prior_captured = prior.captured();
    assert_eq!(
        prior_captured.len(),
        2,
        "prior transport restored — receives the post-fake message"
    );
    assert_eq!(prior_captured[1].to[0].email, "after@example.org");

    // Clean up so subsequent tests don't see prior bound.
    Mail::clear_transport();
}

#[tokio::test]
#[serial]
async fn fake_restores_absent_transport_on_drop() {
    // Start with no transport bound.
    Mail::clear_transport();

    {
        let _fake = Mail::fake();
        Mail::to("alice@example.org")
            .send(Greeting {
                name: "Alice".into(),
            })
            .await
            .unwrap();
        // Drops here.
    }

    // After drop the transport slot must be `None` again, so the next
    // dispatch produces the "no mail transport configured" error.
    let err = Mail::to("alice@example.org")
        .send(Greeting {
            name: "Alice".into(),
        })
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("no mail transport configured"),
        "expected no-transport error after fake drops with no prior: {msg}"
    );

    // The hint must point at the actually-callable path. Operators
    // copying it into their code should not hit a compile error.
    // `Mail::set_transport` is a real method. `bootstrap_from_env` is
    // at `suprnova::mail::boot::bootstrap_from_env()` — NOT on the
    // `Mail` struct directly. Pin both paths so a future hint rewrite
    // can't regress to a non-existent symbol.
    assert!(
        msg.contains("Mail::set_transport"),
        "hint names the set_transport method: {msg}"
    );
    assert!(
        msg.contains("suprnova::mail::boot::bootstrap_from_env"),
        "hint names the bootstrap function at its actual path: {msg}"
    );
    assert!(
        !msg.contains("Mail::bootstrap_from_env"),
        "hint must NOT point at the non-existent `Mail::bootstrap_from_env` path: {msg}"
    );
}

#[tokio::test]
#[serial]
#[should_panic(expected = "expected at least one message matching predicate")]
async fn fake_assert_sent_panics_when_no_match() {
    let fake = Mail::fake();
    Mail::to("alice@example.org")
        .send(Greeting {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    fake.assert_sent(|m| m.subject == "this subject was never sent");
}

#[tokio::test]
#[serial]
#[should_panic(expected = "expected NO message matching predicate")]
async fn fake_assert_not_sent_panics_when_match_exists() {
    let fake = Mail::fake();
    Mail::to("alice@example.org")
        .send(Greeting {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    fake.assert_not_sent(|m| m.subject.contains("Alice"));
}

#[tokio::test]
#[serial]
#[should_panic(expected = "expected 5 message(s), captured 1")]
async fn fake_assert_sent_count_panics_on_mismatch() {
    let fake = Mail::fake();
    Mail::to("alice@example.org")
        .send(Greeting {
            name: "Alice".into(),
        })
        .await
        .unwrap();
    fake.assert_sent_count(5);
}
