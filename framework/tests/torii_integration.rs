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
use tokio::runtime::Runtime;

use suprnova::torii_integration::{init_torii, ToriiConfig};
use suprnova::Auth;

/// One tokio runtime shared across every test in this file.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// One-time Torii initialisation shared across all tests.
///
/// Accessing `SETUP` (via `Lazy::force`) is idempotent and thread-safe.
static SETUP: Lazy<()> = Lazy::new(|| {
    RT.block_on(async {
        let config = ToriiConfig::sqlite_in_memory()
            .await
            .expect("sqlite in-memory connection");
        init_torii(config).await.expect("init_torii");
    });
});

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
        let challenge = Auth::passkey()
            .begin_registration("alice@example.com")
            .await
            .unwrap();

        assert!(!challenge.challenge.is_empty());
        assert_eq!(challenge.user_email, "alice@example.com");
        assert!(!challenge.rp_id.is_empty());
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
        assert!(token.len() >= 16, "token should be a substantial random string");
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
        });

        let kickoff = Auth::oauth("github").begin().await.unwrap();
        assert!(
            kickoff.authorization_url.starts_with("https://github.com/login/oauth"),
            "expected GitHub OAuth URL, got: {}",
            kickoff.authorization_url,
        );
        assert!(!kickoff.state.is_empty());
    });
}
