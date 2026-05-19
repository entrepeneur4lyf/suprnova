//! Phase 11 R3 — cross-facade integration test: failed 2FA verifies
//! and failed recovery-code consumes accumulate toward the
//! BruteForce account lockout. Defense in depth against online
//! brute-force of the 6-digit TOTP search space or the ~40-bit
//! recovery-code space.
//!
//! Unlike `framework/tests/two_factor.rs` (which uses
//! `TestDatabase::fresh` and never inits torii — so the
//! `record_2fa_failure` helper inside TwoFactor swallows
//! "torii not initialised" errors), this file boots BOTH:
//!
//! - torii against a shared in-memory SQLite
//! - the framework's DB pointer at the same connection
//! - the 2FA migration applied manually
//!
//! so the `BruteForce` and `TwoFactor` facades operate on the same
//! database and we can observe the lockout transition.

use once_cell::sync::Lazy;
use sea_orm::Database;
use sea_orm_migration::MigratorTrait;
use serial_test::serial;
use std::sync::OnceLock;
use tokio::runtime::Runtime;

use suprnova::auth_flows::two_factor::migration::Migration as TwoFactorMigration;
use suprnova::auth_flows::two_factor::migration_replay::Migration as TwoFactorReplayMigration;
use suprnova::auth_flows::{BruteForce, TwoFactor, TwoFactorUser};
use suprnova::container::App;
use suprnova::database::DbConnection;
use suprnova::torii_integration::{init_torii, ToriiConfig};
use suprnova::{Crypt, EncryptionKey};

/// One tokio runtime across every test in this file.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// One-time setup: connect to a shared in-memory SQLite, install
/// `DbConnection` in the App container so `DB::connection()` resolves,
/// apply the 2FA migrations manually, and `init_torii` against the
/// same connection (its own migrations cover sessions / users /
/// brute-force-attempts). After this point both facades operate on
/// the same database.
static SETUP: Lazy<()> = Lazy::new(|| {
    static CRYPT_INIT: OnceLock<()> = OnceLock::new();
    CRYPT_INIT.get_or_init(|| {
        Crypt::init(EncryptionKey::generate());
    });

    RT.block_on(async {
        let conn = Database::connect("sqlite:file::memory:?cache=shared")
            .await
            .expect("sqlite in-memory connection");

        // Install in the container so DB::connection() resolves.
        App::singleton(DbConnection::from_raw(conn.clone()));

        // 2FA tables.
        struct TfMigrator;
        impl MigratorTrait for TfMigrator {
            fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
                vec![
                    Box::new(TwoFactorMigration),
                    Box::new(TwoFactorReplayMigration),
                ]
            }
        }
        TfMigrator::up(&conn, None)
            .await
            .expect("two_factor migrations");

        // Torii tables (sessions, users, brute-force attempts, …)
        // installed on the same connection.
        init_torii(ToriiConfig::from_sea_orm(conn).apply_migrations(true))
            .await
            .expect("init_torii");
    });
});

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

fn totp_code_for(otpauth_url: &str) -> String {
    use totp_rs::{Algorithm, Secret, TOTP};
    let url = url::Url::parse(otpauth_url).expect("otpauth url");
    let secret = url
        .query_pairs()
        .find(|(k, _)| k == "secret")
        .map(|(_, v)| v.into_owned())
        .expect("secret query param");
    let secret_bytes = Secret::Encoded(secret).to_bytes().expect("decode secret");
    TOTP::new(Algorithm::SHA1, 6, 1, 30, secret_bytes, None, "label".into())
        .expect("totp")
        .generate_current()
        .expect("generate")
}

#[test]
#[serial]
fn failed_2fa_verifies_lock_the_account() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // Register through torii so the user row exists for the
        // brute-force email lookups.
        suprnova::Auth::password()
            .register("victim-bf-2fa@example.com", "longpassword123")
            .await
            .expect("register");

        let user = FakeUser {
            id: "victim-bf-2fa-uid".into(),
            email: "victim-bf-2fa@example.com".into(),
        };
        let resp = TwoFactor::enroll(&user).await.expect("enroll");
        TwoFactor::confirm(&user, &totp_code_for(&resp.otpauth_url))
            .await
            .expect("confirm");

        // Precondition.
        assert!(
            !BruteForce::is_locked(user.email()).await.unwrap(),
            "freshly-enrolled account must not be locked"
        );

        // 5 wrong codes = default BruteForceProtectionConfig threshold.
        // Each failed verify records a brute-force attempt; the 5th
        // crosses the lockout.
        for _ in 0..5 {
            let _ = TwoFactor::verify(&user, "000000").await.unwrap();
        }

        assert!(
            BruteForce::is_locked(user.email()).await.unwrap(),
            "5 failed 2FA verifies must lock the account via BruteForce"
        );
    });
}

#[test]
#[serial]
fn failed_recovery_code_consumes_lock_the_account() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        suprnova::Auth::password()
            .register("victim-rec-bf@example.com", "longpassword123")
            .await
            .expect("register");

        let user = FakeUser {
            id: "victim-rec-bf-uid".into(),
            email: "victim-rec-bf@example.com".into(),
        };
        let resp = TwoFactor::enroll(&user).await.expect("enroll");
        TwoFactor::confirm(&user, &totp_code_for(&resp.otpauth_url))
            .await
            .expect("confirm");

        // Clear any failed counter from the previous test (#[serial]
        // gives ordering but the static torii instance persists across
        // tests in this binary — a residual counter from a prior file
        // would let this test pass for the wrong reason).
        BruteForce::reset_attempts(user.email()).await.unwrap();
        BruteForce::unlock_account(user.email()).await.unwrap();
        assert!(!BruteForce::is_locked(user.email()).await.unwrap());

        for _ in 0..5 {
            let consumed = TwoFactor::consume_recovery_code(&user, "no-such-code-zzz")
                .await
                .unwrap();
            assert!(!consumed);
        }

        assert!(
            BruteForce::is_locked(user.email()).await.unwrap(),
            "5 failed recovery-code consumes must lock the account via BruteForce"
        );
    });
}

#[test]
#[serial]
fn successful_2fa_verify_resets_failed_attempts() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        suprnova::Auth::password()
            .register("success-resets@example.com", "longpassword123")
            .await
            .expect("register");

        let user = FakeUser {
            id: "success-resets-uid".into(),
            email: "success-resets@example.com".into(),
        };
        let resp = TwoFactor::enroll(&user).await.expect("enroll");
        TwoFactor::confirm(&user, &totp_code_for(&resp.otpauth_url))
            .await
            .expect("confirm");

        // Pile up some failures — but stop one short of lockout.
        BruteForce::reset_attempts(user.email()).await.unwrap();
        BruteForce::unlock_account(user.email()).await.unwrap();
        for _ in 0..4 {
            let _ = TwoFactor::verify(&user, "000000").await.unwrap();
        }
        let status_pre = BruteForce::get_lockout_status(user.email())
            .await
            .unwrap();
        assert!(status_pre.failed_attempts >= 4);
        assert!(!status_pre.is_locked);

        // A successful verify clears the counter.
        let live = totp_code_for(&resp.otpauth_url);
        assert!(TwoFactor::verify(&user, &live).await.unwrap());

        let status_post = BruteForce::get_lockout_status(user.email())
            .await
            .unwrap();
        assert_eq!(
            status_post.failed_attempts, 0,
            "successful verify must reset the failed-attempt counter"
        );
    });
}
