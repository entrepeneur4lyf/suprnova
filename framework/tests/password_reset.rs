//! Phase 11 — `PasswordReset` facade integration tests.
//!
//! Shape mirrors `framework/tests/email_verify.rs` (shared tokio runtime
//! + one-time `init_torii` via `Lazy<()>`). See that file's module docs
//! for the reasoning around the shared runtime and `#[serial]`.
//!
//! Every test in this file runs `#[serial]`, even ones that don't
//! install a `Mail::fake()` themselves: `PasswordReset::complete()`
//! always dispatches the `PasswordChangedMail` security notification
//! through the process-global mail transport. Letting `reset_round_trip`
//! run in parallel with `send_link_silent_for_unknown_email` would
//! let that stray `PasswordChangedMail` land in the other test's fake
//! and break its `assert_sent_count(0)`.

use once_cell::sync::Lazy;
use serial_test::serial;
use tokio::runtime::Runtime;

use suprnova::auth_flows::PasswordReset;
use suprnova::torii_integration::{init_torii, ToriiConfig};

/// One tokio runtime shared across every test in this file.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// One-time Torii initialisation shared across all tests. Also sets
/// `MAIL_FROM` because `PasswordReset::send_link` and `::complete`
/// require it (auth_flows fails closed when the env var is unset —
/// production DMARC/SPF hardening).
static SETUP: Lazy<()> = Lazy::new(|| {
    // SAFETY: tests in this file are `#[serial]`; no parallel reader
    // can observe a torn write. The variable is set once and not
    // mutated again.
    unsafe {
        std::env::set_var("MAIL_FROM", "test-mailer@example.com");
    }
    RT.block_on(async {
        let config = ToriiConfig::sqlite_in_memory()
            .await
            .expect("sqlite in-memory connection")
            .apply_migrations(true);
        init_torii(config).await.expect("init_torii");
    });
});

#[test]
#[serial]
fn reset_round_trip() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // Stub the mail transport so the security notification dispatched
        // inside `complete()` has somewhere to go. Without a transport
        // installed `complete()` would propagate a "no mail transport"
        // error from the security-notification path.
        let _fake = suprnova::mail::Mail::fake();

        let user = suprnova::Auth::password()
            .register("alice-reset@example.com", "longpassword123")
            .await
            .unwrap();

        let (returned_user, token) = PasswordReset::request("alice-reset@example.com")
            .await
            .unwrap()
            .expect("user is on file, request should return Some");
        assert_eq!(returned_user.id, user.id);
        assert!(!token.is_empty(), "request must return a non-empty token");

        // Pre-consumption: verify_token reports valid.
        assert!(PasswordReset::verify_token(&token).await.unwrap());

        // Consume the token and apply the new password.
        let completed = PasswordReset::complete(&token, "freshpassword456")
            .await
            .unwrap();
        assert_eq!(completed.email, "alice-reset@example.com");

        // Old password is rejected.
        assert!(
            suprnova::Auth::password()
                .authenticate(
                    "alice-reset@example.com",
                    "longpassword123",
                    None,
                    None
                )
                .await
                .is_err(),
            "old password must not authenticate after reset"
        );

        // New password is accepted.
        suprnova::Auth::password()
            .authenticate(
                "alice-reset@example.com",
                "freshpassword456",
                None,
                None,
            )
            .await
            .expect("new password must authenticate after reset");

        // Single-use: a second complete on the same token must fail.
        assert!(
            PasswordReset::complete(&token, "anotherpassword789")
                .await
                .is_err(),
            "reset token must not be reusable"
        );
    });
}

#[test]
#[serial]
fn request_for_unknown_email_returns_none() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let result = PasswordReset::request("nobody-reset@example.com")
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "request for unknown email must return Ok(None), not leak existence via Err"
        );
    });
}

#[test]
#[serial]
fn send_link_dispatches_email() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let fake = suprnova::mail::Mail::fake();

        suprnova::Auth::password()
            .register("bob-reset@example.com", "longpassword123")
            .await
            .unwrap();

        PasswordReset::send_link("bob-reset@example.com", "https://app.example.com/reset")
            .await
            .unwrap();

        // The reset link uses Tera's `escape` filter which HTML-encodes
        // forward slashes (per email_verify.rs::send_link_trims_trailing_slash).
        // `?` and `=` are not escaped, so asserting on `token=` in the HTML
        // body is robust.
        fake.assert_sent(|m| {
            m.to.iter().any(|a| a.email == "bob-reset@example.com")
                && m.subject.contains("Reset")
                && m.html
                    .as_deref()
                    .map(|h| h.contains("token="))
                    .unwrap_or(false)
        });
    });
}

#[test]
#[serial]
fn send_link_silent_for_unknown_email() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let fake = suprnova::mail::Mail::fake();

        // Anti-enumeration: send_link returns Ok(()) and dispatches
        // nothing when the email is not on file.
        PasswordReset::send_link(
            "ghost-reset@example.com",
            "https://app.example.com/reset",
        )
        .await
        .expect("send_link must return Ok(()) for unknown emails (anti-enumeration)");

        fake.assert_sent_count(0);
    });
}

#[test]
#[serial]
fn complete_fires_password_changed_email() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // Fake installed BEFORE request/complete so the security
        // notification dispatched inside complete() lands here.
        let fake = suprnova::mail::Mail::fake();

        suprnova::Auth::password()
            .register("charlie-reset@example.com", "longpassword123")
            .await
            .unwrap();

        let (_user, token) = PasswordReset::request("charlie-reset@example.com")
            .await
            .unwrap()
            .expect("user is on file");

        PasswordReset::complete(&token, "freshpassword456")
            .await
            .unwrap();

        // The security notification PasswordChangedMail goes out as
        // a fire-and-forget side-effect of complete(). Its subject is
        // "Your <APP_NAME> password was changed".
        fake.assert_sent(|m| {
            m.to.iter()
                .any(|a| a.email == "charlie-reset@example.com")
                && m.subject.contains("password was changed")
        });
    });
}
