//! Phase 11 — `EmailVerification` facade integration tests.
//!
//! # Design: shared runtime + one-time setup
//!
//! SQLx's in-memory SQLite pool is bound to the tokio `Runtime` it was
//! created on. Each `#[tokio::test]` spawns its own runtime; when that
//! runtime drops, the pool closes. A subsequent test on a new runtime
//! then fails with "no such table" because the global `TORII` `OnceLock`
//! still holds a reference to the stale pool.
//!
//! Additionally, Torii's migrations use `CREATE INDEX IF NOT EXISTS`
//! for some indexes but not all (an upstream quirk). Running
//! `init_torii` twice on the same database therefore panics on the
//! duplicate index. Sharing one runtime + one `init_torii` call across
//! every test body avoids both pitfalls. This mirrors the pattern used
//! by `framework/tests/torii_integration.rs`.
//!
//! # Serial execution
//!
//! `Mail::fake()` swaps the process-global mail transport. With two
//! tests both installing fakes in parallel, the second `fake()` call
//! replaces the first guard's transport, and the first test's
//! dispatched messages land in the second test's recorder. The
//! `#[serial]` attribute (matching the `mail_fake.rs` integration
//! test) serializes every test in this file against the shared
//! transport.

use once_cell::sync::Lazy;
use serial_test::serial;
use tokio::runtime::Runtime;

use suprnova::auth_flows::EmailVerification;
use suprnova::torii_integration::{init_torii, ToriiConfig};

/// One tokio runtime shared across every test in this file.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// One-time Torii initialisation shared across all tests. Also sets
/// `MAIL_FROM` because `EmailVerification::send_link` requires it
/// (auth_flows fails closed when the env var is unset — production
/// DMARC/SPF hardening).
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
fn verify_round_trip() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let user = suprnova::Auth::password()
            .register("alice-verify@example.com", "longpassword123")
            .await
            .unwrap();

        let token = EmailVerification::generate_token(&user.id).await.unwrap();
        let token_str = token
            .token()
            .expect("plaintext available immediately after creation")
            .to_string();

        // Pre-consumption: check() reports valid.
        assert!(EmailVerification::check(&token_str).await.unwrap());

        // verify() consumes the token and stamps email_verified_at.
        let verified = EmailVerification::verify(&token_str).await.unwrap();
        assert_eq!(verified.email, "alice-verify@example.com");
        assert!(
            verified.email_verified_at.is_some(),
            "verify() must stamp email_verified_at"
        );

        // Single-use: a second verify on the same token must fail.
        assert!(EmailVerification::verify(&token_str).await.is_err());
    });
}

#[test]
#[serial]
fn send_link_dispatches_email() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let fake = suprnova::mail::Mail::fake();

        let user = suprnova::Auth::password()
            .register("bob-verify@example.com", "longpassword123")
            .await
            .unwrap();

        EmailVerification::send_link(&user, "https://app.example.com/verify")
            .await
            .unwrap();

        fake.assert_sent(|m| {
            m.to.iter().any(|a| a.email == "bob-verify@example.com")
                && m.subject.contains("Verify")
                && m.html
                    .as_deref()
                    .map(|h| h.contains("token="))
                    .unwrap_or(false)
        });
    });
}

#[test]
#[serial]
fn send_link_trims_trailing_slash() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let fake = suprnova::mail::Mail::fake();

        let user = suprnova::Auth::password()
            .register("carol-verify@example.com", "longpassword123")
            .await
            .unwrap();

        EmailVerification::send_link(&user, "https://app.example.com/verify/")
            .await
            .unwrap();

        // The trailing slash must be stripped so the rendered URL is
        // `https://app.example.com/verify?token=...`, not
        // `.../verify/?token=...`. Tera's `escape` filter HTML-encodes
        // forward slashes in the HTML body (`https:&#x2F;&#x2F;...`),
        // so we assert against the text body which renders verbatim.
        fake.assert_sent(|m| {
            m.text
                .as_deref()
                .map(|t| t.contains("https://app.example.com/verify?token="))
                .unwrap_or(false)
        });
    });
}
