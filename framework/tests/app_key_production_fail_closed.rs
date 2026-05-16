//! Production fail-closed boot when `APP_KEY` is unset (codex review
//! finding #1).
//!
//! `Crypt` is process-global via `OnceLock`, so this scenario lives
//! in its own test binary — it MUST run with no key installed and
//! no key in the environment. If we shared a binary with the
//! dev-key tests, an earlier test would install Crypt and this
//! assertion would become unfalsifiable.
//!
//! Pure-function coverage of the policy decision is in
//! `framework/src/crypto/mod.rs::boot_tests::production_without_key_fails_closed`;
//! this binary's job is to prove the wiring in `Server::from_config`
//! propagates the error verbatim — not just the policy function.

use suprnova::{Router, Server};

#[test]
fn production_without_app_key_returns_err_with_actionable_message() {
    // Set APP_ENV=production and ensure APP_KEY is not set before
    // booting. This binary has its own process; no other test can
    // install Crypt before us.
    //
    // SAFETY: this is the only test in this binary, so no
    // concurrency to coordinate with. The unsafe is for the
    // documented platform race on getenv that doesn't apply at boot.
    unsafe {
        std::env::set_var("APP_ENV", "production");
        std::env::remove_var("APP_KEY");
    }

    let result = Server::from_config(Router::new());
    let err = match result {
        Ok(_) => panic!("production boot without APP_KEY must fail closed"),
        Err(e) => e,
    };
    let msg = format!("{err}");

    assert!(
        msg.contains("APP_KEY is required"),
        "error must clearly state APP_KEY is required, got: {msg}"
    );
    assert!(
        msg.contains("suprnova key:generate"),
        "error must point at the CLI helper, got: {msg}"
    );

    // Cleanup so other downstream tooling that inspects the env
    // doesn't see APP_ENV=production lingering after this test.
    // SAFETY: only test in this binary.
    unsafe {
        std::env::remove_var("APP_ENV");
    }
}
