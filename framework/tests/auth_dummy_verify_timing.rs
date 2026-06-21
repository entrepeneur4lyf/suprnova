//! Timing-oracle regression tests for the auth credential paths.
//!
//! Two enumeration side-channels are guarded here, both observed through a
//! spy [`Hasher`] installed as the process-wide default driver rather than
//! through wall-clock (which is flaky under load):
//!
//! 1. **Passwordless-account login oracle.** When `retrieve_by_credentials`
//!    matches a user whose `get_auth_password()` is `None` and a password
//!    was supplied, `EloquentUserProvider::validate_credentials` must run
//!    the same fixed-cost dummy verify the unknown-user path runs — so an
//!    attacker can't fingerprint "account exists but is passwordless" by
//!    the absence of hash work.
//!
//! 2. **dummy_verify driver coupling.** The default `dummy_verify` must
//!    drive the *configured* hasher, not a hard-coded bcrypt-cost-12 hash —
//!    otherwise under `HASH_DRIVER=argon2id` the dummy (bcrypt) and the real
//!    (argon2) verify cost diverge and enumeration is re-enabled.
//!
//! This file is its own test binary, so the spy driver it installs into the
//! process-wide `DEFAULT_DRIVER` cell never collides with other test files.
//! The spy is installed before any code path resolves the default driver.

use std::any::Any;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, Utc};
use suprnova::hashing::{Algorithm, Hasher};
use suprnova::testing::TestDatabase;
use suprnova::{
    Authenticatable, CanResetPassword, Credentials, EloquentUserProvider, FrameworkError,
    MustVerifyEmail, UserProvider, model,
};

/// A driver that reports a non-bcrypt algorithm and mints an
/// `info::parse`-Unknown hash string, so the facade's `verify_with` routes
/// verification back through *this* driver's `verify` (rather than the
/// built-in bcrypt/argon dispatch). That routing is what lets the test
/// count, deterministically, how many times the configured driver does
/// hash / verify work — the observable stand-in for "did the dummy-verify
/// path run, and did it use the configured hasher?".
struct SpyHasher {
    hashes: Arc<AtomicUsize>,
    verifies: Arc<AtomicUsize>,
}

impl Hasher for SpyHasher {
    fn algorithm(&self) -> Algorithm {
        // Non-bcrypt on purpose: exercises the "configured driver is not
        // bcrypt" case the LOW finding is about.
        Algorithm::Argon2id
    }

    fn hash(&self, _password: &str) -> Result<String, FrameworkError> {
        self.hashes.fetch_add(1, Ordering::SeqCst);
        // Unknown-prefixed so the facade can't route to bcrypt/argon verify
        // and instead delegates to `Self::verify`.
        Ok("spy$dummy".to_string())
    }

    fn verify(&self, _password: &str, _hash: &str) -> Result<bool, FrameworkError> {
        self.verifies.fetch_add(1, Ordering::SeqCst);
        Ok(false)
    }

    fn needs_rehash(&self, _hash: &str) -> bool {
        true
    }
}

/// Serializes the spy-counter critical sections. The spy driver and its
/// counters are process-wide (the default driver is a one-shot), and two
/// tests assert on counter *deltas* / exact equality, so they must not
/// interleave their hash/verify work. Mirrors `auth_session_guard.rs`'s
/// `TEST_LOCK` pattern.
static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Counters shared with the installed spy driver. The default driver is a
/// process-wide one-shot, so the spy is installed exactly once per test
/// binary and every test in this file shares the same counters; the
/// `TEST_LOCK` serialises the delta/equality assertions across them.
static SPY_COUNTERS: OnceLock<(Arc<AtomicUsize>, Arc<AtomicUsize>)> = OnceLock::new();

fn install_spy() -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    SPY_COUNTERS
        .get_or_init(|| {
            let hashes = Arc::new(AtomicUsize::new(0));
            let verifies = Arc::new(AtomicUsize::new(0));
            let driver = SpyHasher {
                hashes: hashes.clone(),
                verifies: verifies.clone(),
            };
            suprnova::hashing::set_default_driver(Box::new(driver))
                .expect("spy driver must install before any hashing call in this binary");
            (hashes, verifies)
        })
        .clone()
}

// A passwordless user model: `get_auth_password()` always returns `None`
// (OAuth / passkey / magic-link account). The `password` column still
// exists so the SeaORM model maps cleanly; it just never surfaces as the
// auth password.
#[model(table = "users", fillable = ["email", "password"])]
pub struct PasswordlessUser {
    pub id: i64,
    pub email: String,
    pub password: String,
    pub email_verified_at: Option<DateTime<Utc>>,
}

impl Authenticatable for PasswordlessUser {
    fn get_auth_identifier(&self) -> String {
        self.id.to_string()
    }
    fn get_auth_password(&self) -> Option<&str> {
        // Passwordless: never expose a hash, so the auth flow must NOT
        // short-circuit but instead run the timing-equalising dummy verify.
        None
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl MustVerifyEmail for PasswordlessUser {
    fn email(&self) -> &str {
        &self.email
    }
    fn email_verified_at(&self) -> Option<DateTime<Utc>> {
        self.email_verified_at
    }
    fn set_email_verified_at(&mut self, v: Option<DateTime<Utc>>) {
        self.email_verified_at = v;
    }
}

impl CanResetPassword for PasswordlessUser {
    fn email_for_reset(&self) -> &str {
        &self.email
    }
    fn set_password_hash(&mut self, hash: &str) {
        self.password = hash.to_string();
    }
}

/// Fresh in-memory DB with a single passwordless user row. The `password`
/// column carries a placeholder (the model maps it) but the user's
/// `get_auth_password()` returns `None`.
async fn setup() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            email TEXT NOT NULL, \
            password TEXT NOT NULL, \
            email_verified_at TEXT\
         )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO users (email, password) VALUES ('oauth@b.com', 'unused-placeholder')",
    )
    .await
    .unwrap();
    db
}

// MED — passwordless account: a supplied password against a matched
// passwordless user must run the SAME dummy verify the unknown-user path
// runs, not short-circuit with zero hash work. Observed via the spy's
// verify counter: without the fix the `(Some, None)` arm returns `Ok(false)`
// with no verify call; with the fix it drives `dummy_verify`, which routes
// through the configured (spy) driver and bumps the counter.
#[tokio::test]
async fn passwordless_login_runs_dummy_verify_like_unknown_user() {
    let _serial = TEST_LOCK.lock().await;
    let (_hashes, verifies) = install_spy();
    let _db = setup().await;
    let p = EloquentUserProvider::<PasswordlessUser>::new();

    let user = p
        .retrieve_by_credentials(&Credentials::password("oauth@b.com", "anything").as_value())
        .await
        .unwrap()
        .expect("passwordless user resolves by email");

    let before = verifies.load(Ordering::SeqCst);
    let ok = p
        .validate_credentials(
            &*user,
            &Credentials::password("oauth@b.com", "anything").as_value(),
        )
        .await
        .unwrap();
    let after = verifies.load(Ordering::SeqCst);

    assert!(!ok, "passwordless account must never validate a password");
    assert!(
        after > before,
        "validate_credentials on a matched passwordless account must run the \
         timing-equalising dummy verify (configured-driver verify count must \
         increase), otherwise the missing hash work fingerprints the account \
         as passwordless"
    );
}

// MED control — when NO password is supplied there is nothing to equalise
// against a real verify, so no dummy work should run (avoids a gratuitous
// hash op on, e.g., a remember-me-only credential set).
#[tokio::test]
async fn passwordless_with_no_password_does_no_dummy_work() {
    let _serial = TEST_LOCK.lock().await;
    let (_hashes, verifies) = install_spy();
    let _db = setup().await;
    let p = EloquentUserProvider::<PasswordlessUser>::new();

    let user = p
        .retrieve_by_credentials(&Credentials::new().insert("email", "oauth@b.com").as_value())
        .await
        .unwrap()
        .expect("passwordless user resolves by email");

    let before = verifies.load(Ordering::SeqCst);
    let ok = p
        .validate_credentials(
            &*user,
            &Credentials::new().insert("email", "oauth@b.com").as_value(),
        )
        .await
        .unwrap();
    let after = verifies.load(Ordering::SeqCst);

    assert!(!ok);
    assert_eq!(
        before, after,
        "no password supplied → no real verify to equalise against → no dummy verify"
    );
}

// LOW — the default dummy_verify must drive the CONFIGURED hasher. Under a
// non-bcrypt driver, the dummy hash is minted by that driver (spy `hash`)
// and verified through that driver (spy `verify`). The old implementation
// hard-coded a bcrypt-cost-12 hash, which would route `verify_with` to the
// built-in bcrypt verify and never touch the configured driver — so both
// spy counters staying at zero would prove the regression.
#[tokio::test]
async fn dummy_verify_uses_configured_hasher_under_non_bcrypt_driver() {
    let _serial = TEST_LOCK.lock().await;
    let (hashes, verifies) = install_spy();
    let _db = setup().await;
    let p = EloquentUserProvider::<PasswordlessUser>::new();

    // Direct trait-default call, independent of the credential flow.
    let result = p.dummy_verify().await.unwrap();
    assert!(!result, "dummy_verify always reports false");

    assert!(
        hashes.load(Ordering::SeqCst) >= 1,
        "the throwaway dummy hash must be minted by the configured driver, \
         not a hard-coded bcrypt constant"
    );
    assert!(
        verifies.load(Ordering::SeqCst) >= 1,
        "dummy_verify must verify through the configured driver so its cost \
         tracks a real verify under HASH_DRIVER=argon2id"
    );
}
