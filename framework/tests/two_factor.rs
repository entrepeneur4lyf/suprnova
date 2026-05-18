//! Phase 11 - `TwoFactor` TOTP integration tests.
//!
//! Each test grabs a fresh in-memory SQLite database via
//! `TestDatabase::fresh::<TestMigrator>()`. The migrator only contains
//! the framework-owned `two_factor::migration::Migration`; the example
//! app wires this into its own `Migrator` in Task 9. `Crypt` is a
//! process-wide `OnceLock`, so we install a key exactly once for the
//! binary (pattern lifted from `framework/tests/pagination.rs`).

use suprnova::auth_flows::two_factor::migration::Migration as TwoFactorMigration;
use suprnova::auth_flows::{TwoFactor, TwoFactorUser};
use suprnova::testing::TestDatabase;
use suprnova::{Crypt, EncryptionKey};

/// `Crypt` is a process-global; install a key exactly once.
fn ensure_crypt() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        Crypt::init(EncryptionKey::generate());
    });
}

/// Test migrator: only ships the framework's 2FA migration so each
/// test starts with a clean `two_factor_credentials` table and no
/// unrelated tables to slow things down.
struct TestMigrator;

#[async_trait::async_trait]
impl sea_orm_migration::MigratorTrait for TestMigrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![Box::new(TwoFactorMigration)]
    }
}

/// Minimal user shape that satisfies the [`TwoFactorUser`] contract.
struct FakeUser {
    id: String,
    email: String,
}

impl TwoFactorUser for FakeUser {
    fn user_id(&self) -> &str {
        &self.id
    }
    fn email(&self) -> &str {
        &self.email
    }
}

/// Compute the live TOTP for an otpauth URL exactly like an
/// authenticator app would. Used inside test bodies to drive the
/// enroll -> confirm -> verify path with a real, valid code.
fn totp_code_for(otpauth_url: &str) -> String {
    use totp_rs::{Algorithm, Secret, TOTP};
    let url = url::Url::parse(otpauth_url).unwrap();
    let secret = url
        .query_pairs()
        .find(|(k, _)| k == "secret")
        .map(|(_, v)| v.into_owned())
        .expect("otpauth url must contain a secret query param");
    let bytes = Secret::Encoded(secret).to_bytes().unwrap();
    TOTP::new(Algorithm::SHA1, 6, 1, 30, bytes, None, "user".into())
        .unwrap()
        .generate_current()
        .unwrap()
}

#[tokio::test]
async fn enrollment_round_trip() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-1".into(),
        email: "alice@example.com".into(),
    };

    let response = TwoFactor::enroll(&user).await.unwrap();
    assert!(
        response.otpauth_url.starts_with("otpauth://totp/"),
        "expected otpauth URL, got {}",
        response.otpauth_url
    );
    assert_eq!(response.recovery_codes.len(), 10);
    assert!(response.qr_code_svg.starts_with("<svg"));
    assert!(response.qr_code_svg.contains("data:image/png;base64,"));

    // Until confirm() runs, 2FA is NOT enabled.
    assert!(!TwoFactor::is_enabled(&user).await.unwrap());
    assert!(
        !TwoFactor::verify(&user, "000000").await.unwrap(),
        "verify() must short-circuit to false before confirm()"
    );

    // Confirm with a real code derived from the otpauth URL.
    let code = totp_code_for(&response.otpauth_url);
    TwoFactor::confirm(&user, &code).await.unwrap();

    assert!(TwoFactor::is_enabled(&user).await.unwrap());

    // verify() with a fresh code must succeed.
    let valid = totp_code_for(&response.otpauth_url);
    assert!(TwoFactor::verify(&user, &valid).await.unwrap());

    // verify() with a clearly invalid code must return Ok(false).
    assert!(!TwoFactor::verify(&user, "000000").await.unwrap());

    // disable() removes the row.
    TwoFactor::disable(&user).await.unwrap();
    assert!(!TwoFactor::is_enabled(&user).await.unwrap());
    assert!(!TwoFactor::verify(&user, &valid).await.unwrap());
}

#[tokio::test]
async fn confirm_with_invalid_code_fails() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-confirm-bad".into(),
        email: "bad@example.com".into(),
    };

    TwoFactor::enroll(&user).await.unwrap();

    let err = TwoFactor::confirm(&user, "000000").await.unwrap_err();
    assert_eq!(err.status_code(), 401);
    // Row still exists, just not confirmed.
    assert!(!TwoFactor::is_enabled(&user).await.unwrap());
}

#[tokio::test]
async fn confirm_without_enrollment_fails() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-no-enroll".into(),
        email: "none@example.com".into(),
    };

    let err = TwoFactor::confirm(&user, "123456").await.unwrap_err();
    assert_eq!(err.status_code(), 401);
}

#[tokio::test]
async fn recovery_code_consume_is_single_use() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-rec".into(),
        email: "rec@example.com".into(),
    };

    let response = TwoFactor::enroll(&user).await.unwrap();
    let code = totp_code_for(&response.otpauth_url);
    TwoFactor::confirm(&user, &code).await.unwrap();

    let first = &response.recovery_codes[0];
    let second = &response.recovery_codes[1];

    // First consume succeeds.
    assert!(TwoFactor::consume_recovery_code(&user, first)
        .await
        .unwrap());

    // Same code cannot be consumed twice.
    assert!(!TwoFactor::consume_recovery_code(&user, first)
        .await
        .unwrap());

    // A different code from the same set still works.
    assert!(TwoFactor::consume_recovery_code(&user, second)
        .await
        .unwrap());

    // A garbage code never works.
    assert!(!TwoFactor::consume_recovery_code(&user, "000000-000000")
        .await
        .unwrap());
}

#[tokio::test]
async fn re_enroll_invalidates_old_recovery_codes_and_resets_confirmed() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-reenroll".into(),
        email: "re@example.com".into(),
    };

    let first = TwoFactor::enroll(&user).await.unwrap();
    let confirm_code = totp_code_for(&first.otpauth_url);
    TwoFactor::confirm(&user, &confirm_code).await.unwrap();
    assert!(TwoFactor::is_enabled(&user).await.unwrap());

    // Re-enroll: the prior row is overwritten, confirmed_at cleared.
    let second = TwoFactor::enroll(&user).await.unwrap();
    assert!(!TwoFactor::is_enabled(&user).await.unwrap());

    // Sanity: the new enrollment must produce a different secret /
    // codes than the first.
    assert_ne!(first.otpauth_url, second.otpauth_url);
    assert_ne!(first.recovery_codes, second.recovery_codes);

    // Confirm the new enrollment so recovery codes become consumable
    // again (the unconfirmed-enrollment lock is exercised in
    // `recovery_codes_locked_until_confirmation`).
    let confirm_two = totp_code_for(&second.otpauth_url);
    TwoFactor::confirm(&user, &confirm_two).await.unwrap();

    // Old recovery codes can no longer be consumed.
    let stale = &first.recovery_codes[0];
    assert!(!TwoFactor::consume_recovery_code(&user, stale).await.unwrap());

    // New recovery codes still work.
    let fresh = &second.recovery_codes[0];
    assert!(TwoFactor::consume_recovery_code(&user, fresh).await.unwrap());
}

#[tokio::test]
async fn verify_returns_false_when_never_enrolled() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-none".into(),
        email: "ghost@example.com".into(),
    };

    assert!(!TwoFactor::is_enabled(&user).await.unwrap());
    assert!(!TwoFactor::verify(&user, "123456").await.unwrap());
}

#[tokio::test]
async fn recovery_codes_locked_until_confirmation() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-unconfirmed-rec".into(),
        email: "pre@example.com".into(),
    };

    // Enroll but DO NOT confirm.
    let response = TwoFactor::enroll(&user).await.unwrap();
    let code = &response.recovery_codes[0];

    // Recovery consumption must mirror verify() and refuse to act on
    // an unconfirmed enrollment - otherwise a recovery code becomes
    // a bypass for the TOTP confirmation step entirely.
    assert!(
        !TwoFactor::consume_recovery_code(&user, code).await.unwrap(),
        "recovery codes must be inert until confirm() runs"
    );

    // After confirm(), the SAME code now works.
    let live = totp_code_for(&response.otpauth_url);
    TwoFactor::confirm(&user, &live).await.unwrap();
    assert!(TwoFactor::consume_recovery_code(&user, code).await.unwrap());
}

#[tokio::test]
async fn disable_event_fires_only_on_real_transition() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use suprnova::auth_flows::events::TwoFactorDisabled;
    use suprnova::events::{EventFacade, Listener};
    use suprnova::FrameworkError;

    // Unique user id so this listener can filter out unrelated
    // TwoFactorDisabled dispatches from other tests sharing the
    // process-global EventDispatcher.
    let user = FakeUser {
        id: "user-event-fire-unique-marker".into(),
        email: "fire@example.com".into(),
    };

    // Spying listener — counts dispatches whose user_id matches the
    // target user. Filters out cross-test noise.
    struct ScopedCounter {
        target_user: String,
        count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Listener<TwoFactorDisabled> for ScopedCounter {
        async fn handle(&self, event: &TwoFactorDisabled) -> Result<(), FrameworkError> {
            if event.user_id == self.target_user {
                self.count.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
    }

    let count = Arc::new(AtomicUsize::new(0));
    let listener: Arc<ScopedCounter> = Arc::new(ScopedCounter {
        target_user: user.id.clone(),
        count: count.clone(),
    });
    EventFacade::listen::<TwoFactorDisabled, _>(listener).await;

    // Disable with no row — must NOT fire the event.
    TwoFactor::disable(&user).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 0);

    // Enroll + confirm + disable — fires exactly once.
    let response = TwoFactor::enroll(&user).await.unwrap();
    let code = totp_code_for(&response.otpauth_url);
    TwoFactor::confirm(&user, &code).await.unwrap();
    TwoFactor::disable(&user).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // Disable again — must NOT fire (no rows affected).
    TwoFactor::disable(&user).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn disable_is_idempotent() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-disable".into(),
        email: "del@example.com".into(),
    };

    // Disable before enrollment: no error, no row, no enabled.
    TwoFactor::disable(&user).await.unwrap();
    assert!(!TwoFactor::is_enabled(&user).await.unwrap());

    // Enroll + confirm + disable.
    let response = TwoFactor::enroll(&user).await.unwrap();
    let code = totp_code_for(&response.otpauth_url);
    TwoFactor::confirm(&user, &code).await.unwrap();
    TwoFactor::disable(&user).await.unwrap();
    assert!(!TwoFactor::is_enabled(&user).await.unwrap());

    // Disable again: still no error.
    TwoFactor::disable(&user).await.unwrap();
}

#[tokio::test]
async fn consuming_all_codes_clears_recovery_column() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-drain".into(),
        email: "drain@example.com".into(),
    };

    let response = TwoFactor::enroll(&user).await.unwrap();
    let code = totp_code_for(&response.otpauth_url);
    TwoFactor::confirm(&user, &code).await.unwrap();

    // Drain every code.
    for c in &response.recovery_codes {
        assert!(TwoFactor::consume_recovery_code(&user, c).await.unwrap());
    }

    // No more matches possible.
    let any = &response.recovery_codes[0];
    assert!(!TwoFactor::consume_recovery_code(&user, any).await.unwrap());

    // verify() still works via TOTP.
    let live = totp_code_for(&response.otpauth_url);
    assert!(TwoFactor::verify(&user, &live).await.unwrap());
}
