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
