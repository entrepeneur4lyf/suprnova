//! Mail boot wiring — covers the env-driven driver-selection matrix.
//!
//! Every test in this file MUST be `#[serial]` because:
//!   1. `MAIL_DRIVER` (and provider creds) are process-global env vars.
//!   2. `Mail::set_transport` writes to a process-global `RwLock`.
//!   3. `MEMORY_CAPTURE` inside `mail::boot` is also process-global.
//!
//! Concurrent tests would clobber each other. `serial_test::serial`
//! serializes them within the crate's test binary; each integration
//! `tests/*.rs` runs as its own process, so env doesn't cross binaries.

use serde::{Deserialize, Serialize};
use serial_test::serial;
use suprnova::async_trait;
use suprnova::mail::{Address, Mail, Mailable};
use tracing_test::traced_test;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct Ping {
    // Tera context requires a JSON object; an empty named struct serializes
    // to `{}`, while a unit struct would serialize to `null` and Tera rejects.
    _placeholder: (),
}

#[async_trait]
impl Mailable for Ping {
    fn mailable_name() -> &'static str {
        "Ping"
    }
    fn subject(&self) -> String {
        "p".into()
    }
    fn text_template_source(&self) -> Option<String> {
        Some("pong".into())
    }
    fn from(&self) -> Option<Address> {
        Some("noreply@suprnova.dev".into())
    }
}

/// Clear every env var this test file touches. Called at the start of each
/// test (defensive against prior-test leakage) and at the end (defensive
/// against future tests in the same process).
fn clear_mail_env() {
    // SAFETY: serial test guarantees no concurrent env mutation.
    unsafe {
        std::env::remove_var("MAIL_DRIVER");
        std::env::remove_var("MAIL_POSTMARK_TOKEN");
        std::env::remove_var("MAIL_POSTMARK_ENDPOINT");
        std::env::remove_var("MAIL_SES_ACCESS_KEY");
        std::env::remove_var("MAIL_SES_SECRET_KEY");
        std::env::remove_var("MAIL_SES_REGION");
        std::env::remove_var("MAIL_SES_ENDPOINT");
        std::env::remove_var("MAIL_SENDGRID_API_KEY");
        std::env::remove_var("MAIL_SENDGRID_ENDPOINT");
        std::env::remove_var("MAIL_MAILGUN_API_KEY");
        std::env::remove_var("MAIL_MAILGUN_DOMAIN");
        std::env::remove_var("MAIL_MAILGUN_ENDPOINT");
        std::env::remove_var("MAIL_RESEND_API_KEY");
        std::env::remove_var("MAIL_RESEND_ENDPOINT");
        std::env::remove_var("MAIL_SMTP_HOST");
        std::env::remove_var("MAIL_SMTP_PORT");
        std::env::remove_var("MAIL_SMTP_USER");
        std::env::remove_var("MAIL_SMTP_PASS");
    }
}

#[tokio::test]
#[serial]
async fn boot_default_binds_log_transport() {
    clear_mail_env();
    Mail::clear_transport();

    suprnova::mail::boot::bootstrap_from_env().unwrap();

    // The log transport is the documented default for "no MAIL_DRIVER set".
    // We assert behavior indirectly: a send must succeed (log transport
    // swallows it).
    Mail::to("alice@example.org")
        .send(Ping::default())
        .await
        .unwrap();

    Mail::clear_transport();
    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_memory_driver_binds_in_memory_transport() {
    clear_mail_env();
    Mail::clear_transport();
    // SAFETY: serial test.
    unsafe {
        std::env::set_var("MAIL_DRIVER", "memory");
    }

    suprnova::mail::boot::bootstrap_from_env().unwrap();

    let captured = suprnova::mail::boot::captured_in_memory();
    assert!(
        captured.is_some(),
        "memory driver must expose its capture buffer"
    );

    // The capture handle must point at the same transport that's been bound
    // globally — verify by sending one message and reading it back.
    Mail::to("alice@example.org")
        .send(Ping::default())
        .await
        .unwrap();
    let messages = captured.unwrap().captured();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].subject, "p");

    Mail::clear_transport();
    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_releases_memory_capture_when_switching_drivers() {
    // Regression: with `OnceLock`, the second `memory` bootstrap would carry
    // the stale Arc from the first. `RwLock<Option<...>>` plus
    // `clear_memory_capture()` at the top of `bootstrap_from_env` must fix
    // this — verify the captured handle is fresh across switches.
    clear_mail_env();
    Mail::clear_transport();

    // First memory bootstrap.
    unsafe {
        std::env::set_var("MAIL_DRIVER", "memory");
    }
    suprnova::mail::boot::bootstrap_from_env().unwrap();
    let first = suprnova::mail::boot::captured_in_memory().unwrap();
    Mail::to("a@example.org")
        .send(Ping::default())
        .await
        .unwrap();
    assert_eq!(first.captured().len(), 1);

    // Switch to log — capture should clear.
    unsafe {
        std::env::set_var("MAIL_DRIVER", "log");
    }
    suprnova::mail::boot::bootstrap_from_env().unwrap();
    assert!(
        suprnova::mail::boot::captured_in_memory().is_none(),
        "non-memory driver must clear the capture handle"
    );

    // Second memory bootstrap — a fresh handle (not the stale `first`).
    unsafe {
        std::env::set_var("MAIL_DRIVER", "memory");
    }
    suprnova::mail::boot::bootstrap_from_env().unwrap();
    let second = suprnova::mail::boot::captured_in_memory().unwrap();
    // The second handle has an empty buffer because it's a NEW transport.
    assert_eq!(
        second.captured().len(),
        0,
        "fresh memory bootstrap must produce an empty buffer"
    );

    Mail::clear_transport();
    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_smtp_driver_binds_unencrypted_when_creds_absent() {
    // We can't actually deliver SMTP in a test, but we CAN verify the
    // bootstrap path runs without error when MAIL_DRIVER=smtp and no creds
    // are set (falls through to unencrypted local-dev mode).
    clear_mail_env();
    Mail::clear_transport();
    unsafe {
        std::env::set_var("MAIL_DRIVER", "smtp");
        std::env::set_var("MAIL_SMTP_HOST", "127.0.0.1");
        std::env::set_var("MAIL_SMTP_PORT", "2525");
    }

    suprnova::mail::boot::bootstrap_from_env().unwrap();

    Mail::clear_transport();
    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_smtp_driver_threads_port_into_authenticated_starttls() {
    // Regression: a prior version of starttls() hardcoded port 587, so an
    // operator setting MAIL_SMTP_PORT alongside USER/PASS had the port
    // silently dropped. This test exercises the authenticated branch and
    // proves bootstrap succeeds with a non-standard port.
    //
    // We can't actually open an SMTP session in-test (no live relay), but
    // building the transport must succeed — the lettre builder validates
    // the host + port shape at construction time.
    clear_mail_env();
    Mail::clear_transport();
    unsafe {
        std::env::set_var("MAIL_DRIVER", "smtp");
        std::env::set_var("MAIL_SMTP_HOST", "smtp.example.com");
        std::env::set_var("MAIL_SMTP_PORT", "2587");
        std::env::set_var("MAIL_SMTP_USER", "submitter");
        std::env::set_var("MAIL_SMTP_PASS", "secret");
    }

    suprnova::mail::boot::bootstrap_from_env().unwrap();

    Mail::clear_transport();
    clear_mail_env();
}

// HTTP-provider smoke tests: each verifies that
//   1. MAIL_DRIVER=<provider> + MAIL_<P>_API_KEY + MAIL_<P>_ENDPOINT
//      → bootstrap_from_env succeeds
//   2. The endpoint override actually routes the subsequent send through
//      the mock server (proving the endpoint env var is wired).

#[tokio::test]
#[serial]
async fn boot_postmark_driver_routes_via_endpoint_override() {
    clear_mail_env();
    Mail::clear_transport();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"MessageID":"x"})),
        )
        .mount(&server)
        .await;

    unsafe {
        std::env::set_var("MAIL_DRIVER", "postmark");
        std::env::set_var("MAIL_POSTMARK_TOKEN", "tok");
        std::env::set_var("MAIL_POSTMARK_ENDPOINT", server.uri());
    }
    suprnova::mail::boot::bootstrap_from_env().unwrap();

    Mail::to("alice@example.org")
        .send(Ping::default())
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "boot wired postmark to the override endpoint"
    );

    Mail::clear_transport();
    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_sendgrid_driver_routes_via_endpoint_override() {
    clear_mail_env();
    Mail::clear_transport();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;

    unsafe {
        std::env::set_var("MAIL_DRIVER", "sendgrid");
        std::env::set_var("MAIL_SENDGRID_API_KEY", "key");
        std::env::set_var("MAIL_SENDGRID_ENDPOINT", server.uri());
    }
    suprnova::mail::boot::bootstrap_from_env().unwrap();

    Mail::to("alice@example.org")
        .send(Ping::default())
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);

    Mail::clear_transport();
    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_mailgun_driver_routes_via_endpoint_override() {
    clear_mail_env();
    Mail::clear_transport();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"x"})))
        .mount(&server)
        .await;

    unsafe {
        std::env::set_var("MAIL_DRIVER", "mailgun");
        std::env::set_var("MAIL_MAILGUN_API_KEY", "key");
        std::env::set_var("MAIL_MAILGUN_DOMAIN", "mg.example.org");
        std::env::set_var("MAIL_MAILGUN_ENDPOINT", server.uri());
    }
    suprnova::mail::boot::bootstrap_from_env().unwrap();

    Mail::to("alice@example.org")
        .send(Ping::default())
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);

    Mail::clear_transport();
    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_resend_driver_routes_via_endpoint_override() {
    clear_mail_env();
    Mail::clear_transport();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"x"})))
        .mount(&server)
        .await;

    unsafe {
        std::env::set_var("MAIL_DRIVER", "resend");
        std::env::set_var("MAIL_RESEND_API_KEY", "key");
        std::env::set_var("MAIL_RESEND_ENDPOINT", server.uri());
    }
    suprnova::mail::boot::bootstrap_from_env().unwrap();

    Mail::to("alice@example.org")
        .send(Ping::default())
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);

    Mail::clear_transport();
    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_ses_driver_routes_via_endpoint_override() {
    clear_mail_env();
    Mail::clear_transport();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("<SendEmailResponse><MessageId>x</MessageId></SendEmailResponse>"),
        )
        .mount(&server)
        .await;

    unsafe {
        std::env::set_var("MAIL_DRIVER", "ses");
        std::env::set_var("MAIL_SES_ACCESS_KEY", "AKIATEST");
        std::env::set_var("MAIL_SES_SECRET_KEY", "secret");
        std::env::set_var("MAIL_SES_REGION", "us-east-1");
        std::env::set_var("MAIL_SES_ENDPOINT", server.uri());
    }
    suprnova::mail::boot::bootstrap_from_env().unwrap();

    Mail::to("alice@example.org")
        .send(Ping::default())
        .await
        .unwrap();
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);

    Mail::clear_transport();
    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_postmark_missing_token_returns_descriptive_error() {
    clear_mail_env();
    Mail::clear_transport();
    unsafe {
        std::env::set_var("MAIL_DRIVER", "postmark");
    }

    let err = suprnova::mail::boot::bootstrap_from_env().unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("MAIL_POSTMARK_TOKEN"),
        "error names env var: {s}"
    );
    assert!(s.contains("postmark"), "error names driver: {s}");

    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_ses_missing_secret_returns_descriptive_error() {
    clear_mail_env();
    Mail::clear_transport();
    unsafe {
        std::env::set_var("MAIL_DRIVER", "ses");
        std::env::set_var("MAIL_SES_ACCESS_KEY", "AKIATEST");
        // intentionally NOT setting MAIL_SES_SECRET_KEY
    }

    let err = suprnova::mail::boot::bootstrap_from_env().unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("MAIL_SES_SECRET_KEY"),
        "error names env var: {s}"
    );
    assert!(s.contains("ses"), "error names driver: {s}");

    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_sendgrid_missing_key_returns_descriptive_error() {
    clear_mail_env();
    Mail::clear_transport();
    unsafe {
        std::env::set_var("MAIL_DRIVER", "sendgrid");
    }

    let err = suprnova::mail::boot::bootstrap_from_env().unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("MAIL_SENDGRID_API_KEY"),
        "error names env var: {s}"
    );
    assert!(s.contains("sendgrid"), "error names driver: {s}");

    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_mailgun_missing_domain_returns_descriptive_error() {
    clear_mail_env();
    Mail::clear_transport();
    unsafe {
        std::env::set_var("MAIL_DRIVER", "mailgun");
        std::env::set_var("MAIL_MAILGUN_API_KEY", "key");
        // intentionally NOT setting MAIL_MAILGUN_DOMAIN
    }

    let err = suprnova::mail::boot::bootstrap_from_env().unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("MAIL_MAILGUN_DOMAIN"),
        "error names env var: {s}"
    );
    assert!(s.contains("mailgun"), "error names driver: {s}");

    clear_mail_env();
}

#[tokio::test]
#[serial]
async fn boot_resend_missing_key_returns_descriptive_error() {
    clear_mail_env();
    Mail::clear_transport();
    unsafe {
        std::env::set_var("MAIL_DRIVER", "resend");
    }

    let err = suprnova::mail::boot::bootstrap_from_env().unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("MAIL_RESEND_API_KEY"),
        "error names env var: {s}"
    );
    assert!(s.contains("resend"), "error names driver: {s}");

    clear_mail_env();
}

#[tokio::test]
#[traced_test]
#[serial]
async fn boot_unknown_driver_falls_back_to_log_with_warning() {
    clear_mail_env();
    Mail::clear_transport();
    unsafe {
        std::env::set_var("MAIL_DRIVER", "bogusdriver");
    }

    suprnova::mail::boot::bootstrap_from_env().unwrap();
    assert!(
        logs_contain("unknown MAIL_DRIVER"),
        "must emit a warn on unknown driver"
    );
    assert!(
        logs_contain("bogusdriver"),
        "warn must include the offending driver name"
    );

    // A subsequent send must succeed via the log fallback.
    Mail::to("alice@example.org")
        .send(Ping::default())
        .await
        .unwrap();

    Mail::clear_transport();
    clear_mail_env();
}

/// Regression pin for the v2 polish: `bootstrap_from_env` MUST be callable
/// from a non-async context. The signature went from `async fn` to `fn`
/// because every supported transport's constructor is sync today — if a
/// future change adds async init and someone flips the signature back to
/// `async fn` without also updating callers, this test fails to compile.
///
/// A `#[test]` (NOT `#[tokio::test]`) calling without `.await` can only
/// compile while the function is sync.
#[test]
#[serial]
fn bootstrap_from_env_is_callable_from_sync_context() {
    clear_mail_env();
    Mail::clear_transport();

    // No `.await`, no tokio runtime — proves the signature is synchronous.
    suprnova::mail::boot::bootstrap_from_env().unwrap();

    Mail::clear_transport();
    clear_mail_env();
}
