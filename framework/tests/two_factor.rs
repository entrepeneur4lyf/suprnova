//! Phase 11 - `TwoFactor` TOTP integration tests.
//!
//! Each test grabs a fresh in-memory SQLite database via
//! `TestDatabase::fresh::<TestMigrator>()`. The migrator only contains
//! the framework-owned `two_factor::migration::Migration`; the example
//! app wires this into its own `Migrator` in Task 9. `Crypt` is a
//! process-wide `OnceLock`, so we install a key exactly once for the
//! binary (pattern lifted from `framework/tests/pagination.rs`).

use suprnova::auth_flows::two_factor::migration::Migration as TwoFactorMigration;
use suprnova::auth_flows::two_factor::migration_replay::Migration as TwoFactorReplayMigration;
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
        vec![
            Box::new(TwoFactorMigration),
            Box::new(TwoFactorReplayMigration),
        ]
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
    assert!(
        TwoFactor::consume_recovery_code(&user, first)
            .await
            .unwrap()
    );

    // Same code cannot be consumed twice.
    assert!(
        !TwoFactor::consume_recovery_code(&user, first)
            .await
            .unwrap()
    );

    // A different code from the same set still works.
    assert!(
        TwoFactor::consume_recovery_code(&user, second)
            .await
            .unwrap()
    );

    // A garbage code never works.
    assert!(
        !TwoFactor::consume_recovery_code(&user, "000000-000000")
            .await
            .unwrap()
    );
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

    // Re-enroll: proof required because the existing enrollment is
    // confirmed. A live TOTP code from the current secret satisfies
    // the proof check; the prior row is then overwritten and
    // confirmed_at cleared.
    let proof = totp_code_for(&first.otpauth_url);
    let second = TwoFactor::re_enroll(&user, &proof).await.unwrap();
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
    assert!(
        !TwoFactor::consume_recovery_code(&user, stale)
            .await
            .unwrap()
    );

    // New recovery codes still work.
    let fresh = &second.recovery_codes[0];
    assert!(
        TwoFactor::consume_recovery_code(&user, fresh)
            .await
            .unwrap()
    );
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

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use suprnova::FrameworkError;
    use suprnova::auth_flows::events::TwoFactorDisabled;
    use suprnova::events::{EventFacade, Listener};

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

#[tokio::test]
async fn verify_rejects_replay_within_same_timestep() {
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();
    ensure_crypt();

    let user = FakeUser {
        id: "replay-uid".into(),
        email: "replay@example.com".into(),
    };

    let response = TwoFactor::enroll(&user).await.unwrap();
    let code = totp_code_for(&response.otpauth_url);

    // Confirm enrollment with the code — this also exercises check_code
    // but the verify-replay path only kicks in for `verify()`.
    TwoFactor::confirm(&user, &code).await.unwrap();

    // First verify after confirmation accepts the current code.
    let live = totp_code_for(&response.otpauth_url);
    assert!(
        TwoFactor::verify(&user, &live).await.unwrap(),
        "first verify on a fresh confirmation must accept the live code"
    );

    // Replay the SAME code immediately. Within the same 30-second
    // window, current_timestep == last_used_timestep — must be
    // rejected even though the code is still structurally valid.
    assert!(
        !TwoFactor::verify(&user, &live).await.unwrap(),
        "replay within the same TOTP timestep must be rejected"
    );

    // A different (fake) code in the same window is also rejected,
    // demonstrating that the rejection is timestep-driven, not just
    // a code-equality check.
    assert!(
        !TwoFactor::verify(&user, "000000").await.unwrap(),
        "any verify in the same timestep as the last successful verify must be rejected"
    );
}

#[tokio::test]
async fn enroll_errors_when_2fa_already_confirmed() {
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();
    ensure_crypt();

    let user = FakeUser {
        id: "block-uid".into(),
        email: "block@example.com".into(),
    };

    // Initial enroll + confirm establishes confirmed 2FA.
    let resp = TwoFactor::enroll(&user).await.unwrap();
    TwoFactor::confirm(&user, &totp_code_for(&resp.otpauth_url))
        .await
        .unwrap();
    assert!(TwoFactor::is_enabled(&user).await.unwrap());

    // A second enroll must NOT silently overwrite the secret — that
    // would let a session-hijacked attacker pivot. Expect 409.
    let err = TwoFactor::enroll(&user).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("already enabled"),
        "error message must mention the already-enabled state; got: {msg}"
    );

    // The original confirmation must still be intact.
    assert!(
        TwoFactor::is_enabled(&user).await.unwrap(),
        "blocked enroll must not have touched the existing row"
    );
}

#[tokio::test]
async fn re_enroll_with_valid_recovery_code_succeeds() {
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();
    ensure_crypt();

    let user = FakeUser {
        id: "rerec-uid".into(),
        email: "rerec@example.com".into(),
    };

    let resp1 = TwoFactor::enroll(&user).await.unwrap();
    TwoFactor::confirm(&user, &totp_code_for(&resp1.otpauth_url))
        .await
        .unwrap();

    // Use a recovery code as proof — it consumes the code in the
    // process, so the same code can't be re-used as proof later.
    let recovery_proof = resp1.recovery_codes[0].clone();
    let resp2 = TwoFactor::re_enroll(&user, &recovery_proof).await.unwrap();

    // New enrollment is pending; old enrollment cleared.
    assert!(!TwoFactor::is_enabled(&user).await.unwrap());
    assert_ne!(resp1.otpauth_url, resp2.otpauth_url);

    // Same recovery code can't be reused (consumed during re_enroll +
    // the new enrollment generated fresh codes anyway).
    TwoFactor::confirm(&user, &totp_code_for(&resp2.otpauth_url))
        .await
        .unwrap();
    assert!(
        !TwoFactor::consume_recovery_code(&user, &recovery_proof)
            .await
            .unwrap(),
        "consumed recovery code must not be reusable, and the new enrollment's codes are different anyway"
    );
}

#[tokio::test]
async fn re_enroll_with_invalid_proof_errors() {
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();
    ensure_crypt();

    let user = FakeUser {
        id: "badproof-uid".into(),
        email: "badproof@example.com".into(),
    };

    let resp = TwoFactor::enroll(&user).await.unwrap();
    TwoFactor::confirm(&user, &totp_code_for(&resp.otpauth_url))
        .await
        .unwrap();

    // Garbage proof — neither a valid TOTP code nor any recovery code.
    let err = TwoFactor::re_enroll(&user, "garbage-proof-xyz")
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("proof"),
        "error message must indicate the proof was rejected; got: {msg}"
    );

    // The original enrollment must still be active.
    assert!(TwoFactor::is_enabled(&user).await.unwrap());
}

#[tokio::test]
async fn re_enroll_without_existing_enrollment_errors() {
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();
    ensure_crypt();

    let user = FakeUser {
        id: "noprior-uid".into(),
        email: "noprior@example.com".into(),
    };

    // No prior enrollment at all.
    let err = TwoFactor::re_enroll(&user, "anything").await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("no confirmed"),
        "error must direct caller to enroll() when no prior enrollment exists; got: {msg}"
    );

    // Pending (unconfirmed) enrollment is also "no confirmed" from
    // re_enroll's perspective.
    TwoFactor::enroll(&user).await.unwrap();
    assert!(!TwoFactor::is_enabled(&user).await.unwrap());
    let err = TwoFactor::re_enroll(&user, "anything").await.unwrap_err();
    assert!(err.to_string().contains("no confirmed"));
}

#[tokio::test]
async fn verify_replay_state_resets_on_re_enrollment() {
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();
    ensure_crypt();

    let user = FakeUser {
        id: "replay-reset-uid".into(),
        email: "replay-reset@example.com".into(),
    };

    // Enroll, confirm, verify (sets last_used_timestep).
    let resp1 = TwoFactor::enroll(&user).await.unwrap();
    TwoFactor::confirm(&user, &totp_code_for(&resp1.otpauth_url))
        .await
        .unwrap();
    let live1 = totp_code_for(&resp1.otpauth_url);
    assert!(TwoFactor::verify(&user, &live1).await.unwrap());

    // Re-enroll wipes the row's last_used_timestep along with the
    // secret — the new secret produces different codes anyway, but
    // even within the same timestep window the new verify path must
    // not inherit the old replay block. Proof here is a recovery
    // code (single-use, doesn't trigger replay protection on its
    // own — TOTP from the old secret would be blocked by the
    // replay check we just installed).
    let recovery_proof = resp1.recovery_codes[0].clone();
    let resp2 = TwoFactor::re_enroll(&user, &recovery_proof).await.unwrap();
    TwoFactor::confirm(&user, &totp_code_for(&resp2.otpauth_url))
        .await
        .unwrap();

    let live2 = totp_code_for(&resp2.otpauth_url);
    assert!(
        TwoFactor::verify(&user, &live2).await.unwrap(),
        "re-enrollment must reset replay state so verify succeeds against the new secret"
    );
}
