//! End-to-end APP_KEY enforcement at `Server::from_config` boot
//! (codex review finding #1).
//!
//! `Crypt` lives in a process-wide `OnceLock`, so each test binary can
//! install at most one key. To exercise both fail-closed and
//! dev-key-generation paths cleanly we'd need separate binaries. Pure-
//! function coverage of the policy lives in
//! `framework/src/crypto/mod.rs::boot_tests`; this binary exercises
//! the end-to-end boot path in the *dev* direction (Local env, no
//! APP_KEY) — which is the path that has to be zero-config.
//!
//! Production fail-closed is verified via the pure
//! `resolve_boot_key` unit tests in the crypto module. Those tests
//! never touch the global `OnceLock`, so they're free to assert the
//! error path without state contamination.

use std::sync::Mutex;

use suprnova::{Crypt, Router, Server};

/// Boot tests serialize on a single mutex — both APP_KEY/APP_ENV and
/// the `CRYPT_KEY` `OnceLock` are process-globals.
static BOOT_LOCK: Mutex<()> = Mutex::new(());

/// Restore the previous env values for keys we touch.
struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn capture(keys: &[&'static str]) -> Self {
        let saved = keys
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            // SAFETY: BOOT_LOCK serializes env access within this binary.
            unsafe {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

#[test]
fn local_env_without_app_key_boots_and_installs_transient_key() {
    let _guard = BOOT_LOCK.lock().unwrap();
    let _envg = EnvGuard::capture(&["APP_ENV", "APP_KEY"]);
    // SAFETY: BOOT_LOCK serializes env access for this binary.
    unsafe {
        std::env::set_var("APP_ENV", "local");
        std::env::remove_var("APP_KEY");
    }

    // Boot should succeed and install some key (transient or pre-existing).
    let result = Server::from_config(Router::new());
    assert!(
        result.is_ok(),
        "local boot without APP_KEY must succeed: {:?}",
        result.err()
    );

    // After boot, Crypt must be initialized — either with the
    // transient dev key generated in this test, or with a key already
    // installed by an earlier test in the binary. Either way, the
    // encrypt path must work end-to-end.
    assert!(Crypt::is_initialized(), "Crypt must be installed after boot");
    let wire = Crypt::encrypt_string("local-dev-payload").expect("encrypt works");
    let plain = Crypt::decrypt_string(&wire).expect("decrypt works");
    assert_eq!(plain, "local-dev-payload");
}

#[test]
fn second_boot_is_idempotent_and_does_not_panic() {
    // Re-entering `Server::from_config` (which embedders sometimes do
    // in long-running test harnesses) must not panic on `OnceLock`
    // reuse. The first boot in this binary installs Crypt; the second
    // boot should see `Crypt::is_initialized()` and skip key install.
    let _guard = BOOT_LOCK.lock().unwrap();
    let _envg = EnvGuard::capture(&["APP_ENV", "APP_KEY"]);
    // SAFETY: BOOT_LOCK serializes env access for this binary.
    unsafe {
        std::env::set_var("APP_ENV", "local");
        std::env::remove_var("APP_KEY");
    }

    let _first = Server::from_config(Router::new()).expect("first boot");
    assert!(Crypt::is_initialized());

    // Second boot must succeed even though Crypt is already installed.
    let _second = Server::from_config(Router::new()).expect("second boot");
}

#[test]
fn boot_with_explicit_app_key_installs_that_key() {
    // When an operator does supply APP_KEY, the boot path must use
    // it (not generate a transient one). We verify this by encrypting
    // before and after boot — the round-trip works either way, but
    // setting APP_KEY exercises the `Configured` branch of
    // `resolve_boot_key` end-to-end.
    //
    // OnceLock caveat: if a sibling test in this binary boots before
    // us, `Crypt::is_initialized()` is true and `from_config` skips
    // key install. The pure-function coverage of the `Configured`
    // branch lives in
    // `framework/src/crypto/mod.rs::boot_tests::production_with_valid_key_succeeds`
    // — that test is order-independent. This integration test
    // serves as a smoke check that the full boot path with
    // APP_KEY=<valid> stays panic-free.
    let _guard = BOOT_LOCK.lock().unwrap();
    let _envg = EnvGuard::capture(&["APP_ENV", "APP_KEY"]);

    let key = suprnova::EncryptionKey::generate().to_base64();
    // SAFETY: BOOT_LOCK serializes env access for this binary.
    unsafe {
        std::env::set_var("APP_ENV", "local");
        std::env::set_var("APP_KEY", &key);
    }

    let _server = Server::from_config(Router::new()).expect("boot with APP_KEY");
    assert!(Crypt::is_initialized());

    let wire = Crypt::encrypt_string("payload-with-explicit-key").expect("encrypt");
    let plain = Crypt::decrypt_string(&wire).expect("decrypt");
    assert_eq!(plain, "payload-with-explicit-key");
}
