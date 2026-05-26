//! Telemetry pins for the `mail.send` span + completion event.
//!
//! Every transport dispatch (Mail::send, MailChannel::deliver, the
//! SendMailJob queue worker) routes through
//! [`suprnova::mail::dispatch_with_telemetry`], which opens a
//! `mail.send` info span carrying the message *shape* and emits a
//! `mail sent` / `mail send failed` event with `duration_ms` on
//! completion. These tests pin that contract so future observability
//! work (Phase 8) can layer richer attributes without losing the
//! baseline schema.
//!
//! All tests are `#[serial]` — the `Mail::TRANSPORT` global is shared.

use serde::{Deserialize, Serialize};
use serial_test::serial;
use suprnova::async_trait;
use suprnova::mail::{Address, Mail, Mailable};
use tracing_test::traced_test;

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
    fn html_template_source(&self) -> Option<String> {
        Some("<p>Welcome aboard, {{ name }}.</p>".into())
    }
    fn from(&self) -> Option<Address> {
        Some("hello@suprnova.dev".into())
    }
}

#[tokio::test]
#[traced_test]
#[serial]
async fn successful_send_emits_mail_send_span_with_shape_fields() {
    let _fake = Mail::fake();

    Mail::to("alice@example.org")
        .cc("team@suprnova.dev")
        .send(Greeting {
            name: "Alice".into(),
        })
        .await
        .unwrap();

    // Span name appears in the formatted output via the
    // `span="mail.send"` field that tracing-subscriber writes when an
    // event is recorded inside a span.
    assert!(
        logs_contain("mail.send"),
        "span name `mail.send` must appear in captured trace output"
    );

    // Transport identity — the in-memory transport reports `"in-memory"`.
    assert!(
        logs_contain("transport=\"in-memory\""),
        "transport name must be on the span fields"
    );

    // Message-shape fields.
    assert!(logs_contain("to_count=1"), "to_count must be captured");
    assert!(logs_contain("cc_count=1"), "cc_count must be captured");
    assert!(logs_contain("has_html=true"), "has_html must be captured");
    assert!(logs_contain("has_text=true"), "has_text must be captured");
    assert!(
        logs_contain("attachment_count=0"),
        "attachment_count must be captured"
    );

    // Completion event — success path.
    assert!(
        logs_contain("mail sent"),
        "completion event message must be present"
    );
    assert!(
        logs_contain("duration_ms="),
        "duration_ms must be on the completion event"
    );
}

#[tokio::test]
#[traced_test]
#[serial]
async fn failed_send_emits_warn_event_with_error_field() {
    // Bind a transport that always fails so dispatch_with_telemetry's
    // warn arm runs — the upstream "no transport" and "empty body"
    // guards short-circuit before the helper, so a failing transport
    // is the only way to exercise the warn path.

    use std::sync::Arc;
    use suprnova::FrameworkError;
    use suprnova::mail::{MailTransport, OutgoingMessage};

    struct AlwaysFailTransport;

    #[async_trait]
    impl MailTransport for AlwaysFailTransport {
        async fn send(&self, _msg: &OutgoingMessage) -> Result<(), FrameworkError> {
            Err(FrameworkError::internal("synthetic transport failure"))
        }
        fn name(&self) -> &'static str {
            "always-fail"
        }
    }

    let _ = Mail::set_transport(Arc::new(AlwaysFailTransport));

    let err = Mail::to("alice@example.org")
        .send(Greeting {
            name: "Alice".into(),
        })
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("synthetic transport failure"),
        "underlying error must propagate from the transport"
    );

    assert!(
        logs_contain("mail.send"),
        "span name still present on failure path"
    );
    assert!(
        logs_contain("transport=\"always-fail\""),
        "transport identity must be on the failure span"
    );
    assert!(
        logs_contain("mail send failed"),
        "failure event message must be emitted"
    );
    assert!(
        logs_contain("duration_ms="),
        "duration_ms must be recorded even on failure"
    );
    assert!(
        logs_contain("synthetic transport failure"),
        "error message must be on the failure event"
    );

    let _ = Mail::clear_transport();
}
