//! Integration tests for Torii-backed authentication.
//!
//! These tests exercise the full stack: `ToriiConfig` → `init_torii` →
//! `Auth::password()` → torii → SeaORM (SQLite in-memory).
//!
//! # Why a single test function
//!
//! Each `#[tokio::test]` runs on its own tokio runtime. SQLx's in-memory
//! SQLite pool binds to the runtime it was created on. When the first
//! runtime shuts down after its test, even though the pool lives in a
//! `static OnceLock`, the underlying connections may be closed (their
//! futures are polled on the original runtime). Subsequent tests on a new
//! runtime then fail with "no such table".
//!
//! The clean fix is to run both auth scenarios inside a single runtime.
//! We use two sequential sub-cases within one `#[tokio::test]` function,
//! using distinct email addresses so the shared state is consistent.

use suprnova::torii_integration::{init_torii, ToriiConfig};
use suprnova::Auth;

/// Full round-trip: register → authenticate (correct) → authenticate (wrong).
///
/// All auth operations run inside a single tokio runtime so the in-memory
/// SQLite pool stays alive and accessible throughout.
#[tokio::test]
async fn password_auth_round_trip() {
    let config = ToriiConfig::sqlite_in_memory().await.unwrap();
    init_torii(config).await.unwrap();

    // ── sub-case 1: register and authenticate with correct password ──────────
    let user = Auth::password()
        .register("test@example.com", "verySecure1!")
        .await
        .unwrap();
    assert_eq!(user.email, "test@example.com");

    let (user2, _session) = Auth::password()
        .authenticate(
            "test@example.com",
            "verySecure1!",
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(user.id, user2.id);

    // ── sub-case 2: wrong password must be rejected ───────────────────────────
    Auth::password()
        .register("wrong@example.com", "correctPassword!")
        .await
        .unwrap();

    let result = Auth::password()
        .authenticate("wrong@example.com", "badPassword", None, None)
        .await;

    assert!(result.is_err());
}
