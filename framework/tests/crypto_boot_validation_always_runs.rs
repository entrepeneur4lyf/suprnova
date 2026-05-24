//! Regression: HIGH audit finding `crypto` #334 — production APP_KEY
//! validation is bypassed after any earlier key install in the same
//! process.
//!
//! Before the fix, `Server::from_config` only ran the APP_KEY env-var
//! validation inside `if !Crypt::is_initialized()`. Any code path that
//! had pre-installed a key (test hook, prior boot, embedder) caused
//! every later boot to skip the validation. An APP_ENV=production boot
//! with a missing/malformed APP_KEY would silently succeed under the
//! transient/test key.
//!
//! The fix moves `resolve_boot_keyring` outside the `is_initialized`
//! guard so validation runs on every boot. Installation remains
//! idempotent (only the first boot calls `init_with_keyring`).
//!
//! This file is a separate test binary so the pre-installed key
//! doesn't leak into other tests in the same process. The pre-install
//! happens once at the top of the test before `Server::from_config`
//! runs.

use suprnova::{Router, Server};

#[test]
fn production_validation_runs_even_when_crypt_is_pre_initialized() {
    // Pre-install a transient key, simulating a test fixture or earlier
    // boot that already populated `CRYPT_RING`.
    let key = suprnova::EncryptionKey::generate();
    let installed = suprnova::crypto::_test_install_key(key);
    assert!(
        installed,
        "this test must run before any other Crypt installer in this binary"
    );

    // SAFETY: this is the only test in this binary; no concurrency.
    // The unsafe is for the documented platform race on getenv that
    // doesn't apply at boot.
    unsafe {
        std::env::set_var("APP_ENV", "production");
        std::env::remove_var("APP_KEY");
    }

    // Before the fix: this would Ok(server) because the `if !is_initialized()`
    // guard skipped the env-var validation entirely.
    // After the fix: Err with the "APP_KEY is required" message because
    // validation now runs unconditionally.
    let result = Server::from_config(Router::new());

    let err = match result {
        Ok(_) => panic!(
            "production boot with pre-initialized Crypt and missing APP_KEY \
             must STILL fail closed — the pre-init must not bypass the env-var \
             validation"
        ),
        Err(e) => e,
    };

    let msg = format!("{err}");
    assert!(
        msg.contains("APP_KEY is required"),
        "error must explain the missing APP_KEY even when Crypt is already \
         initialized; got: {msg}"
    );

    // Cleanup so other tooling that inspects the env doesn't see
    // APP_ENV=production lingering. SAFETY: only test in this binary.
    unsafe {
        std::env::remove_var("APP_ENV");
    }
}
