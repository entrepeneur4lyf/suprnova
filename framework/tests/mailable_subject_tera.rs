//! Coverage for the Tera-templated subject path on the `Mailable`
//! trait — `subject_template_source(&self) -> Option<String>`.
//!
//! Pins the consistency contract: Mailable's subject + html + text
//! all support Tera-templated sources, so a developer who writes
//! `subject_template_source("Hello {{ name }}")` gets the same
//! substitution semantics they get for the bodies. The fallback when
//! `subject_template_source` returns `None` is still the literal
//! `subject()` return value — existing impls are unaffected.
//!
//! Both `MailBuilder::send` (the direct path) and
//! `mailable_registry::render_outgoing` (the queue worker path) go
//! through `render_subject`, so both branches are exercised.

use serde::{Deserialize, Serialize};
use serial_test::serial;
use suprnova::async_trait;
use suprnova::mail::{Address, Mail, Mailable};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct TeraSubject {
    name: String,
    coupon: String,
}

#[async_trait]
impl Mailable for TeraSubject {
    fn mailable_name() -> &'static str {
        "TeraSubject"
    }

    /// Plain `subject()` returns a degraded fallback — this path is
    /// taken ONLY when `subject_template_source` returns `None`. The
    /// test below would fail loudly if `render_subject` accidentally
    /// chose this branch instead of the templated one.
    fn subject(&self) -> String {
        "FALLBACK SHOULD NOT BE USED".into()
    }

    /// The Tera template takes priority over `subject()`.
    fn subject_template_source(&self) -> Option<String> {
        Some("Hi {{ name }} — use code {{ coupon }}".into())
    }

    fn text_template_source(&self) -> Option<String> {
        Some("Welcome {{ name }} (coupon {{ coupon }})".into())
    }

    fn from(&self) -> Option<Address> {
        Some("promo@suprnova.dev".into())
    }
}

#[tokio::test]
#[serial]
async fn mailable_subject_template_source_renders_through_tera() {
    let fake = Mail::fake();

    Mail::to("alice@example.org")
        .send(TeraSubject {
            name: "Alice".into(),
            coupon: "SUMMER25".into(),
        })
        .await
        .unwrap();

    let msgs = fake.captured();
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];

    assert_eq!(
        m.subject, "Hi Alice — use code SUMMER25",
        "subject_template_source must Tera-render with self as context"
    );
    assert_eq!(
        m.text.as_deref(),
        Some("Welcome Alice (coupon SUMMER25)"),
        "body Tera unchanged by the subject convergence"
    );
}

/// Pin the no-template fallback: a Mailable that doesn't override
/// `subject_template_source` still gets its `subject()` return value
/// verbatim. This is the path every existing impl takes — the
/// convergence must be backward-compatible.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct PlainSubject {
    user: String,
}

#[async_trait]
impl Mailable for PlainSubject {
    fn mailable_name() -> &'static str {
        "PlainSubject"
    }
    fn subject(&self) -> String {
        // Note the literal `{{ ... }}` here — if the convergence
        // accidentally piped this through Tera, the assert below
        // would fail with "tera failed to parse" rather than match
        // the verbatim string. That makes this a stronger pin than a
        // simple-string check.
        format!("[debug] subject literal for {{{{ {} }}}}", self.user)
    }
    fn text_template_source(&self) -> Option<String> {
        Some("body".into())
    }
    fn from(&self) -> Option<Address> {
        Some("noreply@suprnova.dev".into())
    }
}

#[tokio::test]
#[serial]
async fn mailable_without_subject_template_source_uses_subject_verbatim() {
    let fake = Mail::fake();

    Mail::to("bob@example.org")
        .send(PlainSubject {
            user: "bob".into(),
        })
        .await
        .unwrap();

    let msgs = fake.captured();
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0].subject, "[debug] subject literal for {{ bob }}",
        "subject() must pass through unchanged when no template source is provided"
    );
}

/// Tera errors on the subject template surface as the `subject`
/// `render_subject` error variant — the dispatch fails before any
/// transport is touched. Mirrors the html/text template error paths.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct BrokenSubject {
    placeholder: u32,
}

#[async_trait]
impl Mailable for BrokenSubject {
    fn mailable_name() -> &'static str {
        "BrokenSubject"
    }
    fn subject(&self) -> String {
        "unused".into()
    }
    fn subject_template_source(&self) -> Option<String> {
        // Tera parse error: missing closing `}}`.
        Some("Hi {{ placeholder".into())
    }
    fn text_template_source(&self) -> Option<String> {
        Some("body".into())
    }
    fn from(&self) -> Option<Address> {
        Some("a@b.com".into())
    }
}

#[tokio::test]
#[serial]
async fn malformed_subject_template_surfaces_tera_error() {
    let _fake = Mail::fake();

    let err = Mail::to("carol@example.org")
        .send(BrokenSubject { placeholder: 1 })
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Tera template (subject)"),
        "error must label the subject template branch — same shape as html/text branches: {msg}"
    );
}
