//! Integration tests for Torii-backed authentication.
//!
//! These tests exercise the full stack: `ToriiConfig` → `init_torii` →
//! `Auth::password()` → torii → SeaORM (SQLite in-memory).
//!
//! # Design: shared runtime + one-time setup
//!
//! SQLx's in-memory SQLite pool is bound to the tokio `Runtime` it was created
//! on. Each `#[tokio::test]` spawns its own runtime; when that runtime drops,
//! the pool closes. A subsequent test on a new runtime then fails with
//! "no such table" because the global `TORII` `OnceLock` still holds a
//! reference to the stale pool.
//!
//! Fix: one `Runtime` shared across all tests via `once_cell::sync::Lazy`.
//!
//! Additionally, Torii's migrations use `CREATE INDEX IF NOT EXISTS` for some
//! indexes but not all (an upstream quirk). Running `init_torii` twice on the
//! same database therefore panics on the duplicate index. `SETUP` ensures the
//! runtime and Torii are both initialised exactly once before any test body
//! runs, regardless of parallel execution order.

use once_cell::sync::Lazy;
use sea_orm_migration::MigratorTrait;
use sea_orm_migration::prelude::*;
use std::sync::Arc;
use tokio::runtime::Runtime;

use suprnova::Auth;
use suprnova::torii_integration::{ToriiConfig, init_torii, middleware::BearerTokenMiddleware};

/// One tokio runtime shared across every test in this file.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// One-time Torii + Suprnova initialisation shared across all tests.
///
/// Accessing `SETUP` (via `Lazy::force`) is idempotent and thread-safe.
///
/// Sets up:
/// - Torii's database via `sqlite_in_memory()` and `init_torii`.
/// - A Suprnova `DbConnection` sharing the same in-memory SQLite database
///   (via the `?cache=shared` URL) so the passkey/OAuth facades can
///   atomically issue/consume single-use ceremony tokens through the
///   `auth_ceremony_tokens` table (ChatGPT audit `torii_integration`
///   HIGH #3).
static SETUP: Lazy<()> = Lazy::new(|| {
    RT.block_on(async {
        let config = ToriiConfig::sqlite_in_memory()
            .await
            .expect("sqlite in-memory connection");
        init_torii(config).await.expect("init_torii");

        // Open a Suprnova DbConnection pointing at the same in-memory
        // SQLite database (same `?cache=shared` URL that
        // ToriiConfig::sqlite_in_memory uses internally), then migrate
        // the auth_ceremony_tokens table. Passkey + OAuth ceremony
        // single-use storage flows through this connection.
        let supr_config = suprnova::database::DatabaseConfig::builder()
            .url("sqlite:file::memory:?cache=shared")
            .max_connections(1)
            .min_connections(1)
            .logging(false)
            .build();
        let supr_conn = suprnova::database::DbConnection::connect(&supr_config)
            .await
            .expect("connect sqlite for ceremony tokens");
        TestMigrator::up(supr_conn.inner(), None)
            .await
            .expect("migrate auth_ceremony_tokens");
        suprnova::App::singleton(supr_conn);
    });
});

/// Local migrator for the auth_ceremony_tokens table. Mirrors the app
/// migration in app/src/migrations/m20251209_000000_*.
struct TestMigrator;

#[async_trait::async_trait]
impl MigratorTrait for TestMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(CreateAuthCeremonyTokensTable)]
    }
}

struct CreateAuthCeremonyTokensTable;

impl MigrationName for CreateAuthCeremonyTokensTable {
    fn name(&self) -> &str {
        "m20251209_000000_create_auth_ceremony_tokens_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateAuthCeremonyTokensTable {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AuthCeremonyTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AuthCeremonyTokens::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(AuthCeremonyTokens::Selector)
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(AuthCeremonyTokens::Kind).string().not_null())
                    .col(
                        ColumnDef::new(AuthCeremonyTokens::Payload)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AuthCeremonyTokens::ExpiresAt)
                            .timestamp()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(AuthCeremonyTokens::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_test_auth_ceremony_tokens_selector")
                    .table(AuthCeremonyTokens::Table)
                    .col(AuthCeremonyTokens::Selector)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AuthCeremonyTokens::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum AuthCeremonyTokens {
    Table,
    Id,
    Selector,
    Kind,
    Payload,
    ExpiresAt,
    CreatedAt,
}

/// Register a user then authenticate with the correct password.
///
/// Verifies the returned `User` IDs match and no error is raised.
#[test]
fn password_register_and_authenticate_round_trip() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let user = Auth::password()
            .register("test@example.com", "verySecure1!")
            .await
            .unwrap();
        assert_eq!(user.email, "test@example.com");

        let (user2, _session) = Auth::password()
            .authenticate("test@example.com", "verySecure1!", None, None)
            .await
            .unwrap();
        assert_eq!(user.id, user2.id);
    });
}

/// Authenticating with the wrong password must return an error.
#[test]
fn wrong_password_fails_authentication() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        Auth::password()
            .register("wrong@example.com", "correctPassword!")
            .await
            .unwrap();

        let result = Auth::password()
            .authenticate("wrong@example.com", "badPassword", None, None)
            .await;

        assert!(result.is_err());
    });
}

/// Passkey registration returns a non-empty challenge, the echoed email, and an rp_id.
///
/// This test does not complete a full WebAuthn round-trip (that requires a browser).
/// It verifies that `begin_registration` wires correctly all the way from
/// `Auth::passkey()` → `Webauthn` → `PasskeyRegistrationChallenge`.
#[test]
fn passkey_registration_challenge_returns_options() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let slot = suprnova::session::new_session_slot_for_test();
        let challenge = suprnova::session::session_scope_for_test(slot, async {
            Auth::passkey()
                .begin_registration("alice@example.com")
                .await
                .unwrap()
        })
        .await;

        assert!(!challenge.challenge.is_empty());
        assert_eq!(challenge.user_email, "alice@example.com");
        assert!(!challenge.rp_id.is_empty());
    });
}

/// Passkey registration MUST refuse to mint a ceremony when no session
/// is mounted. Without a session, finish_registration would never see
/// the selector that begin_registration tried to store, and the
/// ceremony row would orphan in `auth_ceremony_tokens`. Surface the
/// misconfiguration as an actionable internal error.
#[test]
fn passkey_begin_registration_rejects_when_no_session_mounted() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // No session_scope_for_test — call directly so `session_mut`
        // returns None.
        let err = Auth::passkey()
            .begin_registration("no-session@example.com")
            .await
            .expect_err("sessionless begin_registration must error");
        let msg = err.to_string();
        assert!(
            msg.contains("outside a session") && msg.contains("SessionMiddleware"),
            "expected sessionless-misconfig message, got: {msg}"
        );
    });
}

/// Same property for the authentication path: begin_authentication
/// rejects sessionless callers BEFORE touching torii — the email is
/// never looked up, so it doesn't matter whether the user exists.
#[test]
fn passkey_begin_authentication_rejects_when_no_session_mounted() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let err = Auth::passkey()
            .begin_authentication("never-resolved@example.com")
            .await
            .expect_err("sessionless begin_authentication must error");
        let msg = err.to_string();
        assert!(
            msg.contains("outside a session") && msg.contains("SessionMiddleware"),
            "expected sessionless-misconfig message, got: {msg}"
        );
    });
}

/// Same property for the OAuth begin path: sessionless callers
/// receive an actionable internal error instead of an unusable
/// kickoff + orphaned ceremony row.
#[test]
fn oauth_begin_rejects_when_no_session_mounted() {
    Lazy::force(&SETUP);

    Auth::oauth("github").configure(suprnova::torii_integration::oauth::OAuthProviderConfig {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        redirect_url: "http://localhost:8000/auth/oauth/github/callback".into(),
        scopes: vec!["user:email".into()],
        endpoints_override: None,
    });

    RT.block_on(async {
        let err = Auth::oauth("github")
            .begin()
            .await
            .expect_err("sessionless OAuth begin must error");
        let msg = err.to_string();
        assert!(
            msg.contains("outside a session") && msg.contains("SessionMiddleware"),
            "expected sessionless-misconfig message, got: {msg}"
        );
    });
}

/// Magic-link send returns a non-empty, substantial token string.
///
/// Verifies the full path: `Auth::magic_link()` → torii `MagicLinkService` →
/// `get_or_create_user` → token creation → plaintext token returned.
///
/// No mailer is configured, so the call degrades to pure token generation.
/// The callback URL is accepted but not emailed at this phase.
#[test]
fn magic_link_send_returns_token() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let token = Auth::magic_link()
            .send("magic@example.com", "http://localhost:8000/auth/magic")
            .await
            .unwrap();

        assert!(!token.is_empty());
        assert!(
            token.len() >= 16,
            "token should be a substantial random string"
        );
    });
}

/// Magic-link consume returns the expected user and session.
///
/// Calls `send` to obtain a token then `consume` to exchange it for a
/// `(User, Session)`. Asserts the user email matches and the session is
/// linked to the same user. Then verifies the token is single-use: a
/// second `consume` call must fail.
#[test]
fn magic_link_consume_returns_user_and_session() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let email = "magic-consume@example.com";
        let token = Auth::magic_link()
            .send(email, "http://localhost:8000/auth/magic")
            .await
            .unwrap();

        assert!(!token.is_empty());

        let (user, session) = Auth::magic_link().consume(&token).await.unwrap();

        assert_eq!(user.email, email);
        assert_eq!(session.user_id, user.id);

        // Token must be single-use: second consume must fail.
        let second = Auth::magic_link().consume(&token).await;
        assert!(second.is_err(), "magic-link token should be single-use");
    });
}

/// FIX 2 + Codex finding #3: Passkey in-flight state is bound to the
/// begin-time `{state, email, user_id}` ceremony, not just a bare
/// WebAuthn state.
///
/// After ChatGPT audit `torii_integration` HIGH #3, the ceremony's
/// authoritative storage moved from the session payload (race-prone
/// R-M-W) to the `auth_ceremony_tokens` table (atomic single-use via
/// conditional DELETE). The session now carries only the ceremony
/// selector (a UUID); the actual `{email, user_id, state}` payload
/// lives in the DB row.
///
/// This test verifies the new binding:
/// 1. The session stores a ceremony selector under `passkey_reg`.
/// 2. The auth_ceremony_tokens table has exactly one matching row.
/// 3. The row's payload decodes to JSON with `email`, `user_id`, and
///    `state` fields — preserving the cross-email finish defence
///    (codex finding #3).
#[test]
fn passkey_registration_ceremony_stored_in_session() {
    use sea_orm::ColumnTrait;
    use sea_orm::EntityTrait;
    use sea_orm::QueryFilter;

    Lazy::force(&SETUP);

    RT.block_on(async {
        let slot = suprnova::session::new_session_slot_for_test();
        let begin_email = "ceremony-stored@example.com";

        let selector = suprnova::session::session_scope_for_test(slot, async {
            let _challenge = Auth::passkey()
                .begin_registration(begin_email)
                .await
                .expect("begin_registration should succeed");

            suprnova::session::session().and_then(|s| s.get::<String>("passkey_reg"))
        })
        .await
        .expect("begin_registration must store a ceremony selector under 'passkey_reg'");

        assert!(!selector.is_empty(), "stored selector must not be empty");

        // Look up the ceremony row in the auth_ceremony_tokens table.
        // The session stores only the selector; the actual payload
        // lives in the DB where atomic single-use is enforced.
        let conn = suprnova::DB::connection().expect("db connection");
        let row = suprnova::torii_integration::ceremony::entity::Entity::find()
            .filter(
                suprnova::torii_integration::ceremony::entity::Column::Selector
                    .eq(selector.clone()),
            )
            .one(conn.inner())
            .await
            .expect("ceremony lookup query")
            .expect("a ceremony row must exist for the selector stored in the session");

        assert_eq!(
            row.kind, "passkey_register",
            "ceremony row kind must be 'passkey_register'"
        );

        let parsed: serde_json::Value =
            serde_json::from_str(&row.payload).expect("ceremony payload must be valid JSON");

        assert_eq!(
            parsed
                .get("email")
                .and_then(|v| v.as_str())
                .expect("ceremony payload must have an 'email' field"),
            begin_email,
            "stored ceremony email must equal the begin-time email"
        );
        assert!(
            parsed.get("user_id").is_some(),
            "ceremony payload must have a 'user_id' field — proves binding to a specific user"
        );
        assert!(
            parsed.get("state").is_some(),
            "ceremony payload must have a 'state' field — the WebAuthn challenge"
        );
    });
}

/// ChatGPT audit `torii_integration` HIGH #3 — concurrency regression
/// test. Two concurrent passkey-registration finish attempts against
/// the SAME captured ceremony selector must result in exactly one
/// successful consume and one None (ceremony already consumed). The
/// atomic conditional DELETE in `ceremony::consume` is the
/// single-use authority — the loser of the race must NOT also
/// receive the payload.
#[test]
fn ceremony_consume_is_single_use_under_concurrency() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let selector = uuid::Uuid::new_v4().to_string();
        suprnova::torii_integration::ceremony::issue(
            &selector,
            "race_test",
            &serde_json::json!({ "secret": "should-only-be-revealed-once" }),
            10,
        )
        .await
        .expect("issue ceremony");

        // Race: two concurrent consumes against the same selector.
        let sel_a = selector.clone();
        let sel_b = selector.clone();
        let (a, b) = tokio::join!(
            suprnova::torii_integration::ceremony::consume::<serde_json::Value>(
                &sel_a,
                "race_test",
            ),
            suprnova::torii_integration::ceremony::consume::<serde_json::Value>(
                &sel_b,
                "race_test",
            ),
        );
        let r1 = a.expect("racer 1 db ok");
        let r2 = b.expect("racer 2 db ok");

        let success_count = [r1.is_some(), r2.is_some()].iter().filter(|x| **x).count();
        assert_eq!(
            success_count, 1,
            "exactly one consume must succeed; the loser must return None — got r1={r1:?} r2={r2:?}"
        );
    });
}

/// Codex finding #3 — primary regression test for the cross-email finish bug.
///
/// `begin_registration` for `alice@example.com` then `finish_registration`
/// called with `bob@example.com` must reject with a 400 mismatch error.
/// The session ceremony must be consumed even on rejection (a second
/// `finish_registration` call returns "not started or expired").
#[test]
fn passkey_finish_rejects_email_mismatch_with_session_state() {
    use webauthn_rs::prelude::RegisterPublicKeyCredential;

    Lazy::force(&SETUP);

    RT.block_on(async {
        let alice = "alice-mismatch@example.com";
        let bob = "bob-mismatch@example.com";

        let slot = suprnova::session::new_session_slot_for_test();
        let (mismatch_err, second_call_err) =
            suprnova::session::session_scope_for_test(slot, async {
                // Begin a registration ceremony bound to Alice's email.
                Auth::passkey()
                    .begin_registration(alice)
                    .await
                    .expect("begin_registration should succeed");

                // Craft a syntactically-valid (but cryptographically fake)
                // `RegisterPublicKeyCredential`. WebAuthn verification would
                // reject it on signature grounds, but the email-mismatch check
                // happens BEFORE webauthn touches the response — that's the
                // entire point: the mismatch is caught without trusting the
                // ceremony state at all.
                let fake_response: RegisterPublicKeyCredential =
                    serde_json::from_value(serde_json::json!({
                        "id": "AAAAAAA",
                        "rawId": "AAAAAAA",
                        "type": "public-key",
                        "response": {
                            "attestationObject": "AAAAAAA",
                            "clientDataJSON": "AAAAAAA"
                        },
                        "extensions": {}
                    }))
                    .expect("fake RegisterPublicKeyCredential JSON must deserialise");

                let first = Auth::passkey()
                    .finish_registration(bob, fake_response.clone())
                    .await
                    .expect_err("finish_registration with mismatched email must fail");

                // Second call: ceremony must be consumed → expect "not started or expired".
                let second = Auth::passkey()
                    .finish_registration(alice, fake_response)
                    .await
                    .expect_err("second finish_registration must fail — ceremony already consumed");

                (first, second)
            })
            .await;

        assert_eq!(
            mismatch_err.status_code(),
            400,
            "email mismatch must surface as 400 Bad Request, got: status={} msg={}",
            mismatch_err.status_code(),
            mismatch_err,
        );
        let mismatch_msg = mismatch_err.to_string().to_ascii_lowercase();
        assert!(
            mismatch_msg.contains("mismatch"),
            "expected 'mismatch' in error message, got: {mismatch_err}"
        );

        assert_eq!(
            second_call_err.status_code(),
            400,
            "consumed ceremony must surface as 400, got: status={} msg={}",
            second_call_err.status_code(),
            second_call_err,
        );
        let second_msg = second_call_err.to_string().to_ascii_lowercase();
        assert!(
            second_msg.contains("not started") || second_msg.contains("expired"),
            "expected 'not started' or 'expired' (ceremony consumed), got: {second_call_err}"
        );
    });
}

/// Codex finding #3 — email comparison is case-insensitive.
///
/// `begin_registration("Alice@Example.COM")` followed by
/// `finish_registration("alice@example.com", ...)` must accept the email
/// match (and then fail at the webauthn-verification step, not at the
/// mismatch gate). RFC 5321 §2.4 technically permits case-sensitive
/// local-parts, but production email systems uniformly normalise to
/// lowercase, and we follow that convention.
#[test]
fn passkey_finish_email_comparison_is_case_insensitive() {
    use webauthn_rs::prelude::RegisterPublicKeyCredential;

    Lazy::force(&SETUP);

    RT.block_on(async {
        let begin_email = "Casey-Case@Example.COM";
        let finish_email = "casey-case@example.com";

        let slot = suprnova::session::new_session_slot_for_test();
        let err = suprnova::session::session_scope_for_test(slot, async {
            Auth::passkey()
                .begin_registration(begin_email)
                .await
                .expect("begin_registration should succeed");

            let fake_response: RegisterPublicKeyCredential =
                serde_json::from_value(serde_json::json!({
                    "id": "AAAAAAA",
                    "rawId": "AAAAAAA",
                    "type": "public-key",
                    "response": {
                        "attestationObject": "AAAAAAA",
                        "clientDataJSON": "AAAAAAA"
                    },
                    "extensions": {}
                }))
                .expect("fake RegisterPublicKeyCredential JSON must deserialise");

            Auth::passkey()
                .finish_registration(finish_email, fake_response)
                .await
                .expect_err(
                    "finish must still fail — but on webauthn verification, not email mismatch",
                )
        })
        .await;

        // The mismatch gate must NOT be the failure source — the failure
        // must come from later in the pipeline (webauthn rejecting the
        // cryptographically invalid fake response).
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            !msg.contains("email mismatch"),
            "case-only-differing emails must pass the mismatch gate, got: {err}"
        );
    });
}

/// Codex finding #3 — calling `finish_registration` with no prior `begin`
/// returns 400, not 500. The session has no ceremony, so the take_*
/// helper rejects cleanly.
#[test]
fn passkey_finish_missing_session_state_returns_400() {
    use webauthn_rs::prelude::RegisterPublicKeyCredential;

    Lazy::force(&SETUP);

    RT.block_on(async {
        let slot = suprnova::session::new_session_slot_for_test();
        let err = suprnova::session::session_scope_for_test(slot, async {
            let fake_response: RegisterPublicKeyCredential =
                serde_json::from_value(serde_json::json!({
                    "id": "AAAAAAA",
                    "rawId": "AAAAAAA",
                    "type": "public-key",
                    "response": {
                        "attestationObject": "AAAAAAA",
                        "clientDataJSON": "AAAAAAA"
                    },
                    "extensions": {}
                }))
                .expect("fake RegisterPublicKeyCredential JSON must deserialise");

            Auth::passkey()
                .finish_registration("never-began@example.com", fake_response)
                .await
                .expect_err("finish_registration without prior begin must fail")
        })
        .await;

        assert_eq!(
            err.status_code(),
            400,
            "missing ceremony must surface as 400 Bad Request, got: status={} msg={}",
            err.status_code(),
            err,
        );
        let msg = err.to_string().to_ascii_lowercase();
        assert!(
            msg.contains("not started") || msg.contains("expired"),
            "expected 'not started' or 'expired' in message, got: {err}"
        );
    });
}

/// Codex finding #3 — passkey **authentication** must not provision users.
///
/// `Auth::passkey().begin_authentication(...)` against an email that has
/// never been registered must return an error AND must not create a user
/// row. Pre-fix, `find_or_create_user_by_email` was called on every
/// authentication attempt, so probing the API with random emails would
/// silently fill the users table.
#[test]
fn passkey_authentication_does_not_create_user() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let probed = "probe-never-registered@example.com";

        // Sanity: the user must not exist before the test (we use a
        // distinct email so this is robust against shared-fixture noise).
        let exists_before =
            suprnova::torii_integration::user_exists_by_email_test_only(probed)
                .await
                .expect("user_exists_by_email_test_only should not error");
        assert!(
            !exists_before,
            "test fixture invariant: '{probed}' must not exist before the auth attempt"
        );

        let slot = suprnova::session::new_session_slot_for_test();
        let auth_err = suprnova::session::session_scope_for_test(slot, async {
            Auth::passkey()
                .begin_authentication(probed)
                .await
                .expect_err("authentication against an unregistered email must fail")
        })
        .await;

        // Lookup-only auth must surface as 401 (no account), not 500.
        assert_eq!(
            auth_err.status_code(),
            401,
            "passkey authentication against unknown email must surface as 401, got: status={} msg={}",
            auth_err.status_code(),
            auth_err,
        );

        // Critical assertion: the user row must STILL not exist. Pre-fix,
        // `find_or_create_user_by_email` would have inserted a row before
        // failing on "no passkeys". Post-fix uses `find_by_email`, which
        // does not insert.
        let exists_after =
            suprnova::torii_integration::user_exists_by_email_test_only(probed)
                .await
                .expect("user_exists_by_email_test_only should not error");
        assert!(
            !exists_after,
            "passkey authentication must NOT create a user row for '{probed}' — \
             indicates the old find_or_create_user_by_email path is still running on the auth flow"
        );
    });
}

/// FIX 3: `begin_registration` does not create a password row for the user.
///
/// Before the fix, `get_or_create_user_by_email` called `password().register(email, random_uuid)`,
/// which set `password_hash` to a bcrypt-hashed random UUID in the users table.
/// After the fix, user creation goes through `find_or_create_by_email` (the repository
/// layer directly), which creates the users row but leaves `password_hash = NULL`.
///
/// # Discriminator
///
/// We read the raw `password_hash` column after `begin_registration` and assert it is
/// `None`.  Pre-fix code sets a non-null hash (random UUID, bcrypt-hashed); post-fix
/// code leaves the hash null.  Using `password().authenticate()` does NOT discriminate
/// between the two paths (both return `Err` — for different reasons), but a direct hash
/// read does.
#[test]
fn passkey_registration_does_not_create_password_row() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let email = "no-password-row@example.com";

        let slot = suprnova::session::new_session_slot_for_test();
        suprnova::session::session_scope_for_test(slot, async {
            // Creates the user via find_or_create_by_email — no password hash set.
            Auth::passkey()
                .begin_registration(email)
                .await
                .expect("begin_registration should succeed");
        })
        .await;

        // Read the raw password hash from the database.
        // Post-fix: hash is None (password_hash column is NULL).
        // Pre-fix: hash is Some(<bcrypt of random uuid>) — password().register() was called.
        let hash = suprnova::torii_integration::password_hash_for_email_test_only(email)
            .await
            .expect("password_hash_for_email_test_only should not error");

        assert!(
            hash.is_none(),
            "passkey registration must not create a password hash; \
             found hash={hash:?} — indicates the old password().register() path is still running"
        );
    });
}

/// OAuth kickoff returns a valid GitHub authorization URL and a non-empty state token.
#[test]
fn oauth_kickoff_returns_authorization_url() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        Auth::oauth("github").configure(suprnova::torii_integration::oauth::OAuthProviderConfig {
            client_id: "test-client".into(),
            client_secret: "test-secret".into(),
            redirect_url: "http://localhost:8000/auth/oauth/github/callback".into(),
            scopes: vec!["user:email".into()],
            endpoints_override: None,
        });

        let slot = suprnova::session::new_session_slot_for_test();
        let (url, state) = suprnova::session::session_scope_for_test(slot, async {
            let kickoff = Auth::oauth("github").begin().await.unwrap();
            (kickoff.authorization_url, kickoff.state)
        })
        .await;

        assert!(
            url.starts_with("https://github.com/login/oauth"),
            "expected GitHub OAuth URL, got: {url}",
        );
        assert!(!state.is_empty());
    });
}

/// FIX 1a: OAuth CSRF — complete() rejects when the calling session never
/// stored any state (attacker tricks a victim with no in-progress OAuth flow).
///
/// Pre-fix: state was stored in torii's global pkce_verifier store, so any
/// valid state from ANY session in the world would pass validation. Post-fix:
/// state is bound to the session that called begin(); a session with no
/// `oauth_state_<provider>` key rejects any presented state.
#[test]
fn oauth_complete_rejects_when_session_has_no_stored_state() {
    Lazy::force(&SETUP);

    Auth::oauth("github").configure(suprnova::torii_integration::oauth::OAuthProviderConfig {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        redirect_url: "http://localhost:8000/auth/oauth/github/callback".into(),
        scopes: vec!["user:email".into()],
        endpoints_override: None,
    });

    RT.block_on(async {
        // Session B: initiate a flow so a state token exists in B's session.
        // (Pre-fix this also wrote to torii's global store, which is what the
        // attacker would have replayed.)
        let slot_b = suprnova::session::new_session_slot_for_test();
        let state_b = suprnova::session::session_scope_for_test(slot_b, async {
            Auth::oauth("github").begin().await.unwrap().state
        })
        .await;

        // Victim's session: never called begin(). Attempting complete() with
        // any state (including state_b stolen from session B) must fail —
        // there's no `oauth_state_github` key in this session.
        let victim_slot = suprnova::session::new_session_slot_for_test();
        let result = suprnova::session::session_scope_for_test(victim_slot, async {
            Auth::oauth("github").complete("fake-code", &state_b).await
        })
        .await;

        let err = result.expect_err("complete() in a session with no stored state must fail");
        // Codex finding #7: protocol/CSRF failures are caller errors, not
        // server faults. Status must be 400, not 500.
        assert_eq!(
            err.status_code(),
            400,
            "missing state must surface as 400 Bad Request, got: status={} msg={}",
            err.status_code(),
            err,
        );
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("missing"),
            "expected 'missing' error (no oauth_state_github in session), got: {err_msg}",
        );
    });
}

/// FIX 1b: OAuth CSRF — complete() rejects when the session DOES have a stored
/// state but the presented state value doesn't match it.
///
/// This exercises the `Some(expected) if expected != state` branch — the
/// classic state-mismatch CSRF defence. Combined with the "no stored state"
/// test above, both arms of the session check are covered.
#[test]
fn oauth_complete_rejects_when_state_doesnt_match_session_stored() {
    Lazy::force(&SETUP);

    Auth::oauth("github").configure(suprnova::torii_integration::oauth::OAuthProviderConfig {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        redirect_url: "http://localhost:8000/auth/oauth/github/callback".into(),
        scopes: vec!["user:email".into()],
        endpoints_override: None,
    });

    RT.block_on(async {
        // Both begin() and complete() in the SAME session — but complete()
        // receives a state value that does NOT match what begin() stored.
        let slot = suprnova::session::new_session_slot_for_test();
        let result = suprnova::session::session_scope_for_test(slot, async {
            let state_a = Auth::oauth("github").begin().await.unwrap().state;
            assert!(
                !state_a.is_empty(),
                "begin() must produce a non-empty state"
            );

            // Forge a different state — must not match the stored state_a.
            Auth::oauth("github")
                .complete("fake-code", "attacker-controlled-state-value")
                .await
        })
        .await;

        let err =
            result.expect_err("complete() must reject state that doesn't match the stored value");
        // Codex finding #7: CSRF mismatch is a caller error, must be 400.
        assert_eq!(
            err.status_code(),
            400,
            "state mismatch must surface as 400 Bad Request, got: status={} msg={}",
            err.status_code(),
            err,
        );
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("mismatch"),
            "expected 'mismatch' error (state doesn't match session-stored value), got: {err_msg}",
        );
    });
}

// ── PKCE tests (codex review finding #7) ──────────────────────────────────────

/// Codex finding #7a: `begin()` must add `code_challenge` and
/// `code_challenge_method=S256` to the authorize URL. The verifier
/// must also land in the session under `oauth_pkce_verifier_<provider>`.
#[test]
fn oauth_begin_emits_pkce_challenge_and_stores_verifier_in_session() {
    Lazy::force(&SETUP);

    Auth::oauth("github").configure(suprnova::torii_integration::oauth::OAuthProviderConfig {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        redirect_url: "http://localhost:8000/auth/oauth/github/callback".into(),
        scopes: vec!["user:email".into()],
        endpoints_override: None,
    });

    use sea_orm::ColumnTrait;
    use sea_orm::EntityTrait;
    use sea_orm::QueryFilter;

    RT.block_on(async {
        let slot = suprnova::session::new_session_slot_for_test();
        let (url, stored_state) = suprnova::session::session_scope_for_test(slot, async {
            let kickoff = Auth::oauth("github").begin().await.unwrap();
            let state: Option<String> =
                suprnova::session::session().and_then(|s| s.get("oauth_state_github"));
            (kickoff.authorization_url, state)
        })
        .await;

        assert!(
            url.contains("code_challenge="),
            "authorize URL missing code_challenge param: {url}"
        );
        assert!(
            url.contains("code_challenge_method=S256"),
            "authorize URL missing code_challenge_method=S256: {url}"
        );

        let state = stored_state.expect(
            "begin() must still store the CSRF state under oauth_state_github (session binding)",
        );

        // Post-HIGH #3: the PKCE verifier lives in the ceremony table,
        // not the session. The session carries only the state pointer
        // (for the session-binding check); the verifier travels in
        // the ceremony payload that's atomically consumed by complete().
        let conn = suprnova::DB::connection().expect("db connection");
        let row = suprnova::torii_integration::ceremony::entity::Entity::find()
            .filter(
                suprnova::torii_integration::ceremony::entity::Column::Selector.eq(state.clone()),
            )
            .one(conn.inner())
            .await
            .expect("ceremony lookup query")
            .expect("begin() must issue a ceremony row keyed by the state");
        assert_eq!(row.kind, "oauth", "ceremony row kind must be 'oauth'");

        let payload: serde_json::Value =
            serde_json::from_str(&row.payload).expect("ceremony payload must be valid JSON");
        let verifier = payload
            .get("pkce_verifier")
            .and_then(|v| v.as_str())
            .expect("ceremony payload must carry a pkce_verifier field");
        // RFC 7636 §4.1: 43..=128 chars from [A-Za-z0-9-._~]. We use
        // base64url-no-pad, a strict subset that's all in [A-Za-z0-9_-].
        assert!(
            (43..=128).contains(&verifier.len()),
            "verifier length {} not in 43..=128",
            verifier.len()
        );
        assert!(
            verifier
                .chars()
                .all(|c| matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_')),
            "verifier has chars outside [A-Za-z0-9_-]: {verifier}"
        );
    });
}

/// Codex finding #7b: `complete()` reads the verifier from the session
/// and sends it to the token endpoint as `code_verifier=...`. The
/// verifier is one-time use — after `complete()` runs, the session key
/// must be cleared.
///
/// We assert the wire-level behaviour by pointing `endpoints_override`
/// at a one-shot hyper server that captures the form-encoded body and
/// returns the fixture access token. This is the same pattern used in
/// `tests/http_client.rs::spawn_echo`.
#[test]
fn oauth_complete_sends_code_verifier_to_token_endpoint() {
    use http_body_util::{BodyExt, Full};
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::sync::Mutex;

    Lazy::force(&SETUP);

    RT.block_on(async {
        // Captured form bodies from the two upstream calls the OAuth
        // flow makes: (1) POST token, (2) GET userinfo.
        let token_body_captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let userinfo_auth_captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        // Spawn a multi-request hyper server that handles BOTH token and
        // userinfo calls. We pin the body and headers we care about into
        // the shared mutexes, then return canned responses.
        let token_capture = token_body_captured.clone();
        let userinfo_capture = userinfo_auth_captured.clone();
        tokio::spawn(async move {
            // Accept both connections (reqwest may or may not reuse). To
            // be safe we accept in a loop until the test is done; tokio
            // drops this future when the runtime tears down.
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let token_capture = token_capture.clone();
                let userinfo_capture = userinfo_capture.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                        let token_capture = token_capture.clone();
                        let userinfo_capture = userinfo_capture.clone();
                        async move {
                            let path = req.uri().path().to_string();
                            let method = req.method().to_string();
                            if path == "/token" && method == "POST" {
                                let body_bytes =
                                    req.into_body().collect().await.unwrap().to_bytes();
                                let body_str = String::from_utf8_lossy(&body_bytes).to_string();
                                *token_capture.lock().unwrap() = Some(body_str);
                                let payload = serde_json::json!({
                                    "access_token": "fake-access-token",
                                    "token_type": "bearer"
                                });
                                let bytes = serde_json::to_vec(&payload).unwrap();
                                return Ok::<_, Infallible>(
                                    hyper::Response::builder()
                                        .status(200)
                                        .header("content-type", "application/json")
                                        .body(Full::new(bytes::Bytes::from(bytes)))
                                        .unwrap(),
                                );
                            }
                            if path == "/userinfo" {
                                let auth = req
                                    .headers()
                                    .get("authorization")
                                    .and_then(|h| h.to_str().ok())
                                    .map(|s| s.to_string());
                                *userinfo_capture.lock().unwrap() = auth;
                                // An OIDC-conformant provider asserts the
                                // address is verified. Unknown providers now
                                // fail closed without this flag (no emails
                                // endpoint is configured on this test).
                                let payload = serde_json::json!({
                                    "id": 4242,
                                    "login": "pkce-test-user",
                                    "email": "pkce@example.com",
                                    "email_verified": true,
                                    "name": "PKCE Test"
                                });
                                let bytes = serde_json::to_vec(&payload).unwrap();
                                return Ok::<_, Infallible>(
                                    hyper::Response::builder()
                                        .status(200)
                                        .header("content-type", "application/json")
                                        .body(Full::new(bytes::Bytes::from(bytes)))
                                        .unwrap(),
                                );
                            }
                            Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(404)
                                    .body(Full::new(bytes::Bytes::new()))
                                    .unwrap(),
                            )
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });

        // Use a unique provider name so this test's config doesn't
        // collide with the `github` registrations the other tests
        // perform. The provider name only matters for the well-known
        // endpoint table, and we're overriding that anyway.
        let provider_name = "github_pkce_test";
        Auth::oauth(provider_name).configure(
            suprnova::torii_integration::oauth::OAuthProviderConfig {
                client_id: "pkce-client".into(),
                client_secret: "pkce-secret".into(),
                redirect_url: "http://localhost:8000/auth/oauth/cb".into(),
                scopes: vec!["user:email".into()],
                endpoints_override: Some(
                    suprnova::torii_integration::oauth::EndpointOverrides {
                        authorize: format!("{base}/authorize"),
                        token: format!("{base}/token"),
                        userinfo: format!("{base}/userinfo"),
                        emails: None,
                    },
                ),
            },
        );

        use sea_orm::ColumnTrait;
        use sea_orm::EntityTrait;
        use sea_orm::QueryFilter;

        let slot = suprnova::session::new_session_slot_for_test();
        let (result, state_key_after) = suprnova::session::session_scope_for_test(slot, async {
            let kickoff = Auth::oauth(provider_name).begin().await.unwrap();

            // Look up the verifier from the ceremony table BEFORE
            // complete() atomically consumes it. (Post-HIGH #3 the
            // verifier lives in the auth_ceremony_tokens row, not in
            // the session.)
            let conn = suprnova::DB::connection().expect("db connection");
            let row = suprnova::torii_integration::ceremony::entity::Entity::find()
                .filter(
                    suprnova::torii_integration::ceremony::entity::Column::Selector
                        .eq(kickoff.state.clone()),
                )
                .one(conn.inner())
                .await
                .expect("ceremony lookup query")
                .expect("begin() must issue a ceremony row");
            let payload: serde_json::Value =
                serde_json::from_str(&row.payload).expect("ceremony payload must be valid JSON");
            let stored_verifier = payload
                .get("pkce_verifier")
                .and_then(|v| v.as_str())
                .expect("ceremony payload must carry pkce_verifier")
                .to_string();

            let res = Auth::oauth(provider_name).complete("real-auth-code", &kickoff.state).await;

            // After complete() the session state pointer must be
            // cleared (one-time use).
            let after: Option<String> = suprnova::session::session()
                .and_then(|s| s.get(format!("oauth_state_{provider_name}").as_str()));
            (res.map(|_| stored_verifier), after)
        })
        .await;

        let stored_verifier = result.expect("complete() should succeed against the mock provider");
        assert!(
            state_key_after.is_none(),
            "complete() must clear the OAuth state pointer from session — found: {state_key_after:?}"
        );

        let token_body = token_body_captured
            .lock()
            .unwrap()
            .clone()
            .expect("token endpoint must have been hit");
        let expected_verifier_param = format!("code_verifier={stored_verifier}");
        assert!(
            token_body.contains(&expected_verifier_param),
            "token POST body missing the exact code_verifier from the ceremony row.\
             \nexpected: {expected_verifier_param}\nbody:     {token_body}"
        );
        assert!(
            token_body.contains("grant_type=authorization_code"),
            "token POST body missing grant_type=authorization_code: {token_body}"
        );
        assert!(
            token_body.contains("code=real-auth-code"),
            "token POST body missing the original auth code: {token_body}"
        );

        assert_eq!(
            userinfo_auth_captured.lock().unwrap().as_deref(),
            Some("Bearer fake-access-token"),
            "userinfo call must use the bearer token returned by the token endpoint"
        );
    });
}

/// ChatGPT audit `torii_integration` HIGH #3 — replay protection.
///
/// After a successful `complete()` consumes the ceremony, a second
/// `complete()` with the same state value must fail. This proves the
/// atomic single-use property at the OAuth facade level.
///
/// (Replaces the prior `_returns_400_when_pkce_verifier_missing_from_session`
/// test, which probed the old session-state design where the verifier
/// lived under a separate key. Post-HIGH #3, state and verifier are
/// atomically linked in one ceremony row, so they cannot be partially
/// missing — but the row can be consumed once and then gone.)
#[test]
fn oauth_complete_rejects_state_replay_after_ceremony_consumed() {
    Lazy::force(&SETUP);

    Auth::oauth("github").configure(suprnova::torii_integration::oauth::OAuthProviderConfig {
        client_id: "test-client".into(),
        client_secret: "test-secret".into(),
        redirect_url: "http://localhost:8000/auth/oauth/github/callback".into(),
        scopes: vec!["user:email".into()],
        endpoints_override: None,
    });

    use sea_orm::ColumnTrait;
    use sea_orm::EntityTrait;
    use sea_orm::QueryFilter;

    RT.block_on(async {
        let slot = suprnova::session::new_session_slot_for_test();
        let result = suprnova::session::session_scope_for_test(slot, async {
            let state = Auth::oauth("github").begin().await.unwrap().state;

            // Simulate the ceremony having been consumed by a prior
            // (concurrent) `complete()` call: directly delete the row
            // from the auth_ceremony_tokens table.
            let conn = suprnova::DB::connection().expect("db connection");
            let dr = suprnova::torii_integration::ceremony::entity::Entity::delete_many()
                .filter(
                    suprnova::torii_integration::ceremony::entity::Column::Selector
                        .eq(state.clone()),
                )
                .exec(conn.inner())
                .await
                .expect("manual delete (simulating prior consume)");
            assert_eq!(dr.rows_affected, 1, "ceremony row must have existed");

            // The session-bound state is still set; complete() passes
            // the session check but the atomic ceremony consume now
            // returns None → 400.
            Auth::oauth("github").complete("any-code", &state).await
        })
        .await;

        let err = result.expect_err("complete() must fail when the ceremony row is gone");
        assert_eq!(
            err.status_code(),
            400,
            "replayed/consumed state must surface as 400 Bad Request, got: status={} msg={}",
            err.status_code(),
            err,
        );
        let err_msg = err.to_string();
        assert!(
            err_msg.to_ascii_lowercase().contains("already consumed")
                || err_msg.to_ascii_lowercase().contains("replay"),
            "expected 'already consumed' or 'replay' in error message, got: {err_msg}"
        );
    });
}

// ── BearerTokenMiddleware tests ───────────────────────────────────────────────

/// Creates a real `suprnova::Request` with an optional `Authorization` header
/// by spinning up a minimal in-memory HTTP/1.1 connection.
///
/// `suprnova::Request` wraps `hyper::Request<hyper::body::Incoming>`, and
/// `Incoming` can only be produced by hyper's connection machinery. We use a
/// `tokio::io::duplex` pipe + `hyper::server::conn::http1` to parse a raw
/// HTTP request, giving us a genuine `Incoming` body without a network socket.
async fn build_request_async(auth_header: Option<&str>) -> suprnova::Request {
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use std::convert::Infallible;
    use tokio::sync::oneshot;

    let (req_tx, req_rx) = oneshot::channel::<suprnova::Request>();

    // Build the raw HTTP bytes to send over the wire.
    let auth_line = auth_header
        .map(|h| format!("Authorization: {}\r\n", h))
        .unwrap_or_default();
    let http_bytes = format!(
        "GET /api/test HTTP/1.1\r\nHost: localhost\r\n{}Content-Length: 0\r\n\r\n",
        auth_line
    );

    let (client_io, server_io) = tokio::io::duplex(4096);

    // Server side: accept one request, send it through the oneshot channel.
    // `service_fn` requires `Fn`, so we use a `Mutex<Option<_>>` to move
    // the sender out on the first (and only) call.
    let req_tx = std::sync::Mutex::new(Some(req_tx));
    tokio::spawn(async move {
        let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
            let wrapped = suprnova::Request::new(req);
            if let Ok(mut guard) = req_tx.lock()
                && let Some(tx) = guard.take()
            {
                let _ = tx.send(wrapped);
            }
            async {
                Ok::<_, Infallible>(hyper::Response::new(
                    http_body_util::Empty::<bytes::Bytes>::new(),
                ))
            }
        });

        let _ = http1::Builder::new()
            .serve_connection(hyper_util::rt::TokioIo::new(server_io), svc)
            .await;
    });

    // Client side: write the raw request bytes, then drop (signals EOF).
    {
        use tokio::io::AsyncWriteExt;
        let mut client = client_io;
        client.write_all(http_bytes.as_bytes()).await.unwrap();
    }

    req_rx
        .await
        .expect("server should have received the request")
}

/// `BearerTokenMiddleware` binds the session when a valid token is presented.
///
/// Registers a user, authenticates (obtaining a real session token), then
/// drives the middleware with that token in the `Authorization` header.
/// Asserts that `Auth::check()` returns `true` inside the session scope.
#[test]
fn bearer_token_middleware_binds_session_when_token_valid() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // Register + authenticate to get a real session token.
        Auth::password()
            .register("bearer-valid@example.com", "Bearer1!")
            .await
            .unwrap();

        let (_user, torii_session) = Auth::password()
            .authenticate("bearer-valid@example.com", "Bearer1!", None, None)
            .await
            .unwrap();

        // Freshly authenticated sessions always carry the plaintext token —
        // `None` is reserved for sessions loaded from storage (hash only).
        // The fork's `SessionToken::Display` is redacted to "[REDACTED]";
        // `expose_secret()` is the explicit accessor for the underlying value.
        let token_str = torii_session
            .token
            .as_ref()
            .expect("freshly authenticated session must carry plaintext token")
            .expose_secret()
            .to_string();
        assert!(!token_str.is_empty());

        // Build a fake request with the bearer token.
        let request = build_request_async(Some(&format!("Bearer {}", token_str))).await;

        // Set up a per-request session scope (as SessionMiddleware would do at runtime).
        let slot = suprnova::session::new_session_slot_for_test();

        let authenticated = suprnova::session::session_scope_for_test(slot, async {
            // Stub `next` that just returns OK without touching the session.
            let next: suprnova::Next =
                Arc::new(|_req| Box::pin(async { Ok(suprnova::HttpResponse::text("ok")) }));

            let mw = BearerTokenMiddleware;
            use suprnova::Middleware;
            let _response = mw.handle(request, next).await;

            // After middleware runs, `Auth::check()` must return true because
            // `set_auth_user` was called with the raw torii UserId string.
            Auth::check()
        })
        .await;

        assert!(
            authenticated,
            "BearerTokenMiddleware should bind the session for a valid token"
        );
    });
}

/// `BearerTokenMiddleware` stores the raw torii `UserId` string, not a hash.
///
/// Registers a user, authenticates, drives the middleware, then asserts that
/// `Auth::id()` returns the raw `"usr_…"` prefixed string — proving that the
/// old FNV-1a hashing punt has been removed.
#[test]
fn bearer_middleware_stores_raw_user_id_not_hash() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        Auth::password()
            .register("raw-uid@example.com", "RawUid1!")
            .await
            .unwrap();

        let (_user, torii_session) = Auth::password()
            .authenticate("raw-uid@example.com", "RawUid1!", None, None)
            .await
            .unwrap();

        // Freshly authenticated sessions always carry the plaintext token —
        // `None` is reserved for sessions loaded from storage (hash only).
        // The fork's `SessionToken::Display` is redacted to "[REDACTED]";
        // `expose_secret()` is the explicit accessor for the underlying value.
        let token_str = torii_session
            .token
            .as_ref()
            .expect("freshly authenticated session must carry plaintext token")
            .expose_secret()
            .to_string();

        let request = build_request_async(Some(&format!("Bearer {}", token_str))).await;
        let slot = suprnova::session::new_session_slot_for_test();

        let session_uid = suprnova::session::session_scope_for_test(slot, async {
            let next: suprnova::Next =
                Arc::new(|_req| Box::pin(async { Ok(suprnova::HttpResponse::text("ok")) }));

            let mw = BearerTokenMiddleware;
            use suprnova::Middleware;
            let _response = mw.handle(request, next).await;

            Auth::id()
        })
        .await;

        let session_uid = session_uid.expect("Auth::id() should be Some after bearer middleware");
        assert!(
            session_uid.starts_with("usr_"),
            "expected raw torii UserId (starts with 'usr_'), got: {session_uid}"
        );
    });
}

/// `BearerTokenMiddleware` passes through without binding when the token is invalid.
///
/// Drives the middleware with a garbage token; asserts that `Auth::check()`
/// returns `false` (no session was bound) and the request reached the handler
/// (response is `Ok`, not `401`).
#[test]
fn bearer_token_middleware_passes_through_when_token_invalid() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let request = build_request_async(Some("Bearer garbage_invalid_token_xyz")).await;

        let slot = suprnova::session::new_session_slot_for_test();

        let (authenticated, response_ok) = suprnova::session::session_scope_for_test(slot, async {
            let next: suprnova::Next =
                Arc::new(|_req| Box::pin(async { Ok(suprnova::HttpResponse::text("ok")) }));

            let mw = BearerTokenMiddleware;
            use suprnova::Middleware;
            let response = mw.handle(request, next).await;

            (Auth::check(), response.is_ok())
        })
        .await;

        assert!(
            !authenticated,
            "BearerTokenMiddleware should NOT bind the session for an invalid token"
        );
        assert!(
            response_ok,
            "BearerTokenMiddleware should pass through (no 401) for an invalid token"
        );
    });
}

/// When the GitHub `/user` endpoint omits the primary email (private
/// email accounts), `complete()` must NOT fall back to `login` or the
/// opaque provider id. Instead, it must consult `/user/emails` and
/// pick the row marked `primary: true, verified: true`.
#[test]
fn oauth_complete_falls_back_to_user_emails_when_userinfo_email_omitted() {
    use http_body_util::{BodyExt, Full};
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;

    Lazy::force(&SETUP);

    RT.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(|req: hyper::Request<hyper::body::Incoming>| async move {
                        let path = req.uri().path().to_string();
                        let method = req.method().to_string();
                        if path == "/token" && method == "POST" {
                            let _ = req.into_body().collect().await;
                            let payload = serde_json::json!({
                                "access_token": "github-private-email-token",
                                "token_type": "bearer",
                            });
                            return Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/json")
                                    .body(Full::new(bytes::Bytes::from(
                                        serde_json::to_vec(&payload).unwrap(),
                                    )))
                                    .unwrap(),
                            );
                        }
                        if path == "/userinfo" {
                            // Simulate a GitHub `/user` response from a
                            // private-email account: id is present,
                            // login is present, email is omitted.
                            let payload = serde_json::json!({
                                "id": 9001,
                                "login": "private-email-user",
                                "name": "Private Email User"
                            });
                            return Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/json")
                                    .body(Full::new(bytes::Bytes::from(
                                        serde_json::to_vec(&payload).unwrap(),
                                    )))
                                    .unwrap(),
                            );
                        }
                        if path == "/emails" {
                            // GitHub `/user/emails` shape: list of
                            // `{ email, primary, verified, visibility }`.
                            let payload = serde_json::json!([
                                {
                                    "email": "noisy-secondary@example.com",
                                    "primary": false,
                                    "verified": true,
                                    "visibility": null,
                                },
                                {
                                    "email": "verified-primary@example.com",
                                    "primary": true,
                                    "verified": true,
                                    "visibility": "private",
                                },
                            ]);
                            return Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/json")
                                    .body(Full::new(bytes::Bytes::from(
                                        serde_json::to_vec(&payload).unwrap(),
                                    )))
                                    .unwrap(),
                            );
                        }
                        Ok::<_, Infallible>(
                            hyper::Response::builder()
                                .status(404)
                                .body(Full::new(bytes::Bytes::new()))
                                .unwrap(),
                        )
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });

        // Use a unique provider name to avoid polluting the github
        // registration the other tests rely on. Hardcode the GitHub
        // identifier on the override so the user-facing config doesn't
        // need to know about the emails endpoint.
        let provider_name = "github_private_email_test";
        Auth::oauth(provider_name).configure(
            suprnova::torii_integration::oauth::OAuthProviderConfig {
                client_id: "private-email-client".into(),
                client_secret: "private-email-secret".into(),
                redirect_url: "http://localhost:8000/auth/oauth/cb".into(),
                scopes: vec!["user:email".into()],
                endpoints_override: Some(suprnova::torii_integration::oauth::EndpointOverrides {
                    authorize: format!("{base}/authorize"),
                    token: format!("{base}/token"),
                    userinfo: format!("{base}/userinfo"),
                    emails: Some(format!("{base}/emails")),
                }),
            },
        );

        let slot = suprnova::session::new_session_slot_for_test();
        let user = suprnova::session::session_scope_for_test(slot, async {
            let kickoff = Auth::oauth(provider_name).begin().await.unwrap();
            let (user, _session) = Auth::oauth(provider_name)
                .complete("real-code", &kickoff.state)
                .await
                .expect("complete must succeed and use the emails-endpoint primary");
            user
        })
        .await;

        assert_eq!(
            user.email, "verified-primary@example.com",
            "must pick the primary verified email, not a username or opaque id"
        );
    });
}

/// When neither `/user` nor `/user/emails` yields a verified
/// address, `complete()` must REFUSE the callback with a 502 rather
/// than persist a username or opaque id in the email column. This is
/// the core regression guard for the MEDIUM finding: non-email
/// fallbacks must never make it to torii.
#[test]
fn oauth_complete_refuses_when_no_verified_email_available() {
    use http_body_util::{BodyExt, Full};
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;

    Lazy::force(&SETUP);

    RT.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(|req: hyper::Request<hyper::body::Incoming>| async move {
                        let path = req.uri().path().to_string();
                        let method = req.method().to_string();
                        if path == "/token" && method == "POST" {
                            let _ = req.into_body().collect().await;
                            let payload = serde_json::json!({
                                "access_token": "no-email-token",
                                "token_type": "bearer",
                            });
                            return Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/json")
                                    .body(Full::new(bytes::Bytes::from(
                                        serde_json::to_vec(&payload).unwrap(),
                                    )))
                                    .unwrap(),
                            );
                        }
                        if path == "/userinfo" {
                            // No email field present in the payload.
                            let payload = serde_json::json!({
                                "id": 4242,
                                "login": "noemail",
                            });
                            return Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/json")
                                    .body(Full::new(bytes::Bytes::from(
                                        serde_json::to_vec(&payload).unwrap(),
                                    )))
                                    .unwrap(),
                            );
                        }
                        if path == "/emails" {
                            // Only unverified rows — the helper must
                            // reject and the OAuth flow must refuse.
                            let payload = serde_json::json!([
                                {
                                    "email": "unverified@example.com",
                                    "primary": true,
                                    "verified": false,
                                    "visibility": null,
                                }
                            ]);
                            return Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/json")
                                    .body(Full::new(bytes::Bytes::from(
                                        serde_json::to_vec(&payload).unwrap(),
                                    )))
                                    .unwrap(),
                            );
                        }
                        Ok::<_, Infallible>(
                            hyper::Response::builder()
                                .status(404)
                                .body(Full::new(bytes::Bytes::new()))
                                .unwrap(),
                        )
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });

        let provider_name = "github_no_email_test";
        Auth::oauth(provider_name).configure(
            suprnova::torii_integration::oauth::OAuthProviderConfig {
                client_id: "no-email-client".into(),
                client_secret: "no-email-secret".into(),
                redirect_url: "http://localhost:8000/auth/oauth/cb".into(),
                scopes: vec!["user:email".into()],
                endpoints_override: Some(suprnova::torii_integration::oauth::EndpointOverrides {
                    authorize: format!("{base}/authorize"),
                    token: format!("{base}/token"),
                    userinfo: format!("{base}/userinfo"),
                    emails: Some(format!("{base}/emails")),
                }),
            },
        );

        let slot = suprnova::session::new_session_slot_for_test();
        let err = suprnova::session::session_scope_for_test(slot, async {
            let kickoff = Auth::oauth(provider_name).begin().await.unwrap();
            Auth::oauth(provider_name)
                .complete("real-code", &kickoff.state)
                .await
        })
        .await
        .expect_err("complete must refuse when no verified email is available");

        assert_eq!(
            err.status_code(),
            502,
            "no verified email must map to 502 (bad upstream), got {}: {}",
            err.status_code(),
            err,
        );
        let msg = err.to_string();
        assert!(
            msg.contains("verified email"),
            "expected verified-email error message, got: {msg}"
        );
    });
}

/// Fail-closed default for unknown providers: a userinfo payload that
/// carries an `email` but NO `email_verified` flag must NOT be trusted.
/// Before the fix the unknown-provider arm defaulted an absent flag to
/// `true`, so a provider returning an unverified address (or a hostile
/// userinfo endpoint) could link to — or take over — an account keyed on
/// that email. With no verified-emails endpoint to fall back to, the flow
/// must refuse with a 502. This is the regression guard for the flip: it
/// fails if the unknown-provider default is ever reverted to `true`.
#[test]
fn oauth_complete_refuses_unflagged_userinfo_email_from_unknown_provider() {
    use http_body_util::{BodyExt, Full};
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;

    Lazy::force(&SETUP);

    RT.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");

        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(|req: hyper::Request<hyper::body::Incoming>| async move {
                        let path = req.uri().path().to_string();
                        let method = req.method().to_string();
                        if path == "/token" && method == "POST" {
                            let _ = req.into_body().collect().await;
                            let payload = serde_json::json!({
                                "access_token": "unflagged-token",
                                "token_type": "bearer",
                            });
                            return Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/json")
                                    .body(Full::new(bytes::Bytes::from(
                                        serde_json::to_vec(&payload).unwrap(),
                                    )))
                                    .unwrap(),
                            );
                        }
                        if path == "/userinfo" {
                            // Email present, but NO `email_verified` flag, and
                            // the provider name is unknown to the framework.
                            // Fail-closed: this address must not be trusted.
                            let payload = serde_json::json!({
                                "id": 7777,
                                "login": "unflagged",
                                "email": "unflagged@example.com",
                                "name": "Unflagged User"
                            });
                            return Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/json")
                                    .body(Full::new(bytes::Bytes::from(
                                        serde_json::to_vec(&payload).unwrap(),
                                    )))
                                    .unwrap(),
                            );
                        }
                        Ok::<_, Infallible>(
                            hyper::Response::builder()
                                .status(404)
                                .body(Full::new(bytes::Bytes::new()))
                                .unwrap(),
                        )
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });

        // Unknown provider name + no emails endpoint: the only address on
        // offer is the unflagged userinfo `email`. Under fail-closed it is
        // rejected, and there is no verified-emails fallback to recover it.
        let provider_name = "oidc_unflagged_email_test";
        Auth::oauth(provider_name).configure(
            suprnova::torii_integration::oauth::OAuthProviderConfig {
                client_id: "unflagged-client".into(),
                client_secret: "unflagged-secret".into(),
                redirect_url: "http://localhost:8000/auth/oauth/cb".into(),
                scopes: vec!["user:email".into()],
                endpoints_override: Some(suprnova::torii_integration::oauth::EndpointOverrides {
                    authorize: format!("{base}/authorize"),
                    token: format!("{base}/token"),
                    userinfo: format!("{base}/userinfo"),
                    emails: None,
                }),
            },
        );

        let slot = suprnova::session::new_session_slot_for_test();
        let err = suprnova::session::session_scope_for_test(slot, async {
            let kickoff = Auth::oauth(provider_name).begin().await.unwrap();
            Auth::oauth(provider_name)
                .complete("real-code", &kickoff.state)
                .await
        })
        .await
        .expect_err("an unknown provider's unflagged userinfo email must be refused");

        assert_eq!(
            err.status_code(),
            502,
            "unflagged userinfo email must map to 502 (bad upstream), got {}: {}",
            err.status_code(),
            err,
        );
        let msg = err.to_string();
        assert!(
            msg.contains("verified email"),
            "expected verified-email error message, got: {msg}"
        );
    });
}
