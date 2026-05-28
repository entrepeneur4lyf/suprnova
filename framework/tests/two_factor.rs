//! `TwoFactor` TOTP integration tests.
//!
//! Each test grabs a fresh in-memory SQLite database via
//! `TestDatabase::fresh::<TestMigrator>()`. The migrator only contains
//! the framework-owned `two_factor::migration::Migration`; consumer
//! apps wire this into their own `Migrator`. `Crypt` is a
//! process-wide `OnceLock`, so we install a key exactly once for the
//! binary (pattern lifted from `framework/tests/pagination.rs`).

use sea_orm_migration::prelude::*;
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

/// Test migrator: ships the framework's 2FA migrations + a local
/// `remember_tokens` table so `TwoFactor::start_challenge`'s
/// remember-me revoke has a table to DELETE from. (The framework
/// does not ship the remember-me migration — consumer apps own that
/// schema — so we recreate the canonical shape here.)
struct TestMigrator;

#[async_trait::async_trait]
impl sea_orm_migration::MigratorTrait for TestMigrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![
            Box::new(TwoFactorMigration),
            Box::new(TwoFactorReplayMigration),
            Box::new(CreateRememberTokensTable),
        ]
    }
}

/// Local migration mirroring the canonical remember-me schema from
/// `framework/tests/remember_me.rs`. `TwoFactor::start_challenge`
/// revokes remember-me tokens for the user before demoting to
/// pending — without this table that revoke errors out, and tests
/// that pre-set `auth_user_id` (to prove `start_challenge` clears
/// it) hit the revoke path.
struct CreateRememberTokensTable;

impl MigrationName for CreateRememberTokensTable {
    fn name(&self) -> &str {
        "m20240101_000002_create_remember_tokens_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateRememberTokensTable {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RememberTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RememberTokens::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(RememberTokens::UserId).string().not_null())
                    .col(ColumnDef::new(RememberTokens::Selector).string().not_null())
                    .col(
                        ColumnDef::new(RememberTokens::TokenHash)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RememberTokens::ExpiresAt)
                            .timestamp()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RememberTokens::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(RememberTokens::LastUsedAt)
                            .timestamp()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_two_factor_test_remember_tokens_selector")
                    .table(RememberTokens::Table)
                    .col(RememberTokens::Selector)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RememberTokens::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum RememberTokens {
    Table,
    Id,
    UserId,
    Selector,
    TokenHash,
    ExpiresAt,
    CreatedAt,
    LastUsedAt,
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
async fn start_challenge_sets_pending_and_clears_auth_user() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let slot = suprnova::session::new_session_slot_for_test();
    suprnova::session::session_scope_for_test(slot, async {
        suprnova::session::set_auth_user("pre-existing-id");
        assert_eq!(
            suprnova::session::auth_user_id(),
            Some("pre-existing-id".to_string()),
            "auth slot must be set before start_challenge for the test to be meaningful"
        );

        TwoFactor::start_challenge("test-user-1").await.unwrap();

        // Pending is now the target id.
        assert_eq!(
            TwoFactor::pending_user_id(),
            Some("test-user-1".to_string()),
        );
        // Auth slot was cleared — pending and authed are mutually
        // exclusive.
        assert_eq!(suprnova::session::auth_user_id(), None);
    })
    .await;
}

#[tokio::test]
async fn cancel_challenge_clears_pending() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let slot = suprnova::session::new_session_slot_for_test();
    suprnova::session::session_scope_for_test(slot, async {
        TwoFactor::start_challenge("test-user-cancel")
            .await
            .unwrap();
        assert!(TwoFactor::pending_user_id().is_some());

        TwoFactor::cancel_challenge();

        assert_eq!(
            TwoFactor::pending_user_id(),
            None,
            "cancel_challenge must clear the pending slot"
        );
        // cancel does NOT install the user as authed — that's the
        // whole point of cancelling.
        assert_eq!(suprnova::session::auth_user_id(), None);
    })
    .await;
}

#[tokio::test]
async fn complete_challenge_without_pending_returns_400() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let slot = suprnova::session::new_session_slot_for_test();
    suprnova::session::session_scope_for_test(slot, async {
        // No pending — complete_challenge must fail closed with 400.
        let err = TwoFactor::complete_challenge("000000").await.unwrap_err();
        assert_eq!(
            err.status_code(),
            400,
            "complete_challenge without pending must be 400 Bad Request"
        );
    })
    .await;
}

#[tokio::test]
async fn pending_user_id_outside_session_returns_none() {
    // Outside a `session_scope_for_test` the session task-local is
    // not installed; pending must read as None and start/cancel
    // must no-op silently — they cannot crash. `start_challenge`'s
    // remember-me revoke also no-ops here because `Auth::id()`
    // returns None outside any session/request scope.
    assert_eq!(TwoFactor::pending_user_id(), None);
    TwoFactor::start_challenge("orphan-id").await.unwrap();
    assert_eq!(TwoFactor::pending_user_id(), None);
    TwoFactor::cancel_challenge();
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
async fn regenerate_recovery_codes_requires_confirmation() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-regen-pre-confirm".into(),
        email: "regen-pre@example.com".into(),
    };

    // No enrollment at all: regenerate must 400 with the
    // "no confirmed 2FA enrollment" guard.
    let err = TwoFactor::regenerate_recovery_codes(&user, "anything")
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 400);

    // Enroll but don't confirm: still not "enabled," so the guard
    // refuses regenerate.
    let _ = TwoFactor::enroll(&user).await.unwrap();
    let err = TwoFactor::regenerate_recovery_codes(&user, "anything")
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 400);
}

#[tokio::test]
async fn regenerate_recovery_codes_with_recovery_proof_consumes_it() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-regen-rec".into(),
        email: "regen-rec@example.com".into(),
    };

    let response = TwoFactor::enroll(&user).await.unwrap();
    let confirm_code = totp_code_for(&response.otpauth_url);
    TwoFactor::confirm(&user, &confirm_code).await.unwrap();
    let original_codes = response.recovery_codes.clone();

    // Use one of the original recovery codes as proof. The recovery
    // path is symmetric with re_enroll's proof model — the code is
    // single-use, so it's consumed.
    let proof = original_codes[0].clone();
    let fresh = TwoFactor::regenerate_recovery_codes(&user, &proof)
        .await
        .unwrap();

    assert_eq!(fresh.len(), 10);
    let original_set: std::collections::HashSet<_> = original_codes.iter().collect();
    let fresh_set: std::collections::HashSet<_> = fresh.iter().collect();
    assert!(original_set.is_disjoint(&fresh_set));

    // The proof code was burned BEFORE rotation, so even though it
    // appeared in the original set it can't be reused against the new
    // row.
    assert!(
        !TwoFactor::consume_recovery_code(&user, &proof)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn regenerate_recovery_codes_rejects_invalid_proof() {
    ensure_crypt();
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();

    let user = FakeUser {
        id: "user-regen-bad".into(),
        email: "regen-bad@example.com".into(),
    };

    let response = TwoFactor::enroll(&user).await.unwrap();
    let confirm_code = totp_code_for(&response.otpauth_url);
    TwoFactor::confirm(&user, &confirm_code).await.unwrap();
    let original_codes = response.recovery_codes.clone();

    let err = TwoFactor::regenerate_recovery_codes(&user, "definitely-not-a-code")
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 401);

    // The original recovery codes are untouched after a rejected
    // attempt — a hostile caller that fails proof cannot blow away
    // the legitimate codes. Skim-test on the first one (consume
    // returns true if the code matches the persisted set).
    let first = original_codes.first().expect("enrollment yields ≥1 code");
    let consumed = TwoFactor::consume_recovery_code(&user, first)
        .await
        .unwrap();
    assert!(
        consumed,
        "original recovery code {first} must still work after failed-proof attempt"
    );
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

// Regression gate for the verify replay *race* (not just sequential
// replay, which `verify_rejects_replay_within_same_timestep` covers).
//
// `tokio::join!` polls all five verifies on this one task — `tokio::spawn`
// is deliberately avoided because the DB connection is task-local and a
// spawned task would not inherit it. The single-connection test pool
// serves their `find` queries FIFO, so all five read
// `last_used_timestep = NULL` and clear the fast-path pre-check *before*
// any of them reaches the stamping UPDATE. That is precisely the TOCTOU
// window: the old read-modify-write returned five `true`s here, because
// each task independently stamped the row. The atomic conditional UPDATE
// lets only the first claimant flip the column; the other four match zero
// rows and return `false`. So this asserts exactly-one-success, which is
// a hard fail for the pre-fix code on any backend.
#[tokio::test]
async fn concurrent_verifies_in_same_timestep_elect_one_winner() {
    let _db = TestDatabase::fresh::<TestMigrator>().await.unwrap();
    ensure_crypt();

    let user = FakeUser {
        id: "race-uid".into(),
        email: "race@example.com".into(),
    };

    let response = TwoFactor::enroll(&user).await.unwrap();
    TwoFactor::confirm(&user, &totp_code_for(&response.otpauth_url))
        .await
        .unwrap();

    let live = totp_code_for(&response.otpauth_url);
    let (r1, r2, r3, r4, r5) = tokio::join!(
        TwoFactor::verify(&user, &live),
        TwoFactor::verify(&user, &live),
        TwoFactor::verify(&user, &live),
        TwoFactor::verify(&user, &live),
        TwoFactor::verify(&user, &live),
    );

    let results = [r1, r2, r3, r4, r5];
    assert!(
        results.iter().all(Result::is_ok),
        "no concurrent verify should error: {results:?}"
    );
    let successes = results.iter().filter(|r| matches!(r, Ok(true))).count();
    assert_eq!(
        successes, 1,
        "exactly one concurrent verify may claim the timestep; got {successes} in {results:?}"
    );

    // The timestep is now claimed, so a later verify with the same code
    // in the same window is rejected like any other replay.
    assert!(
        !TwoFactor::verify(&user, &live).await.unwrap(),
        "post-race verify of the same code in the same timestep must be rejected"
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
