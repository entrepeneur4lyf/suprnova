//! Torii authentication integration for Suprnova
//!
//! This module integrates the [Torii](https://github.com/cmackenzie1/torii-rs) authentication
//! library into the Suprnova framework, providing a Laravel-like API for password-based,
//! OAuth, passkey, and magic-link authentication.
//!
//! # API deviation from original plan
//!
//! The plan assumed a `ToriiBuilder` with `.with_storage()` / `.with_seaorm_connection()`.
//! The **published** `torii 0.5.3` (resolves from `^0.5.2`) has a simpler API:
//!
//! - No `ToriiBuilder` exists — use `Torii::new(Arc<repositories>)` directly.
//! - Migrations are run via `SeaORMStorage::migrate()` before building the `Torii` instance.
//! - This is consistent with the examples in the published crate's own doc-tests.

pub mod magic_link;
pub mod middleware;
pub mod oauth;
pub mod passkey;
pub mod password;

use std::sync::{Arc, OnceLock};

use torii::seaorm::SeaORMRepositoryProvider;
use torii::Torii;
use torii_core::repositories::{
    PasswordRepository, PasswordRepositoryProvider, UserRepository, UserRepositoryProvider,
};
use torii_storage_seaorm::SeaORMStorage;

use crate::error::FrameworkError;

// Re-export torii's User and Session so consumers only depend on suprnova::*.
// LockoutStatus surfaces through `auth_flows::BruteForce` return types, so
// consumers that hold and inspect a status (e.g. a controller branching on
// `status.failed_attempts`) can do so without adding `torii` as a direct
// dependency.
pub use torii::{LockoutStatus, Session, SessionToken, User, UserId};

/// The single global Torii instance, pinned to the SeaORM repository provider.
static TORII: OnceLock<Torii<SeaORMRepositoryProvider>> = OnceLock::new();

/// The raw repository provider — stored separately so internal code can call
/// `find_or_create_by_email` without going through `password().register()`.
/// `Torii<R>` does not expose a public `repositories()` accessor, so we keep
/// our own `Arc` from the moment we build the provider in `init_torii`.
static PROVIDER: OnceLock<Arc<SeaORMRepositoryProvider>> = OnceLock::new();

/// Find or create a user by email using the repository layer directly.
///
/// This is the correct way to get-or-create a user without creating a dummy
/// password row. Called from passkey **registration** (which legitimately
/// provisions a new user the first time the email registers a passkey).
///
/// **Do not call this from authentication / login paths.** Login flows must
/// use [`find_user_by_email_lookup_only`] so failed attempts cannot silently
/// create accounts. (Codex review finding #3.)
pub(crate) async fn find_or_create_user_by_email(email: &str) -> Result<User, FrameworkError> {
    let provider = PROVIDER
        .get()
        .ok_or_else(|| FrameworkError::internal("Torii not initialised. Call init_torii() first."))?;
    provider
        .user()
        .find_or_create_by_email(email)
        .await
        .map_err(|e| FrameworkError::internal(format!("find_or_create_user_by_email: {e}")))
}

/// Look up a user by email. Returns `Ok(None)` if the user doesn't exist —
/// the caller decides what to do with the absence.
///
/// Use this for authentication / login flows. **Never** use
/// [`find_or_create_user_by_email`] in login paths: it would silently create
/// accounts from failed login attempts (account-enumeration / probing
/// footgun, codex review finding #3).
pub(crate) async fn find_user_by_email_lookup_only(
    email: &str,
) -> Result<Option<User>, FrameworkError> {
    let provider = PROVIDER
        .get()
        .ok_or_else(|| FrameworkError::internal("Torii not initialised. Call init_torii() first."))?;
    provider
        .user()
        .find_by_email(email)
        .await
        .map_err(|e| FrameworkError::internal(format!("find_user_by_email_lookup_only: {e}")))
}

/// Test-only helper: returns `true` if a user row exists for this email.
///
/// # Purpose
///
/// Integration tests need to assert that authentication paths do **not**
/// create user rows on failed login attempts (codex review finding #3).
/// `password_hash_for_email_test_only` can't discriminate "no user" from
/// "user with NULL password hash" — both return `Ok(None)`. This helper
/// answers the existence question directly.
///
/// Hidden from documentation to discourage accidental production use.
#[doc(hidden)]
pub async fn user_exists_by_email_test_only(email: &str) -> Result<bool, FrameworkError> {
    Ok(find_user_by_email_lookup_only(email).await?.is_some())
}

/// Return the stored password hash for a user identified by email, or `None`
/// if the user has no password row.
///
/// # Purpose
///
/// This function exists **only for integration tests** that need to verify
/// that passkey registration does not create a password row.  Production code
/// should never need to inspect raw password hashes; use the `Auth::password()`
/// facade instead.
///
/// The function is `pub` (not `pub(crate)`) so integration tests in
/// `framework/tests/` — which compile as separate crates — can access it.
/// It is hidden from documentation to discourage accidental use.
#[doc(hidden)]
pub async fn password_hash_for_email_test_only(
    email: &str,
) -> Result<Option<String>, FrameworkError> {
    let provider = PROVIDER
        .get()
        .ok_or_else(|| FrameworkError::internal("Torii not initialised. Call init_torii() first."))?;
    // First find the user; if the user doesn't exist return None (no row means no hash).
    let user = match provider.user().find_by_email(email).await {
        Ok(Some(u)) => u,
        Ok(None) => return Ok(None),
        Err(e) => {
            return Err(FrameworkError::internal(format!(
                "password_hash_for_email_test_only find_by_email: {e}"
            )))
        }
    };
    provider
        .password()
        .get_password_hash(&user.id)
        .await
        .map_err(|e| {
            FrameworkError::internal(format!(
                "password_hash_for_email_test_only get_password_hash: {e}"
            ))
        })
}

/// Configuration for initialising Torii authentication.
///
/// Create one with [`ToriiConfig::from_sea_orm`] (typical) or
/// [`ToriiConfig::sqlite_in_memory`] (tests/dev).
///
/// # Passkey configuration
///
/// Set `passkey_rp_id` and `passkey_rp_origin` to enable WebAuthn/passkey
/// authentication. Both default to `"localhost"` / `"http://localhost"` so tests
/// and local development work without extra configuration.
pub struct ToriiConfig {
    conn: sea_orm::DatabaseConnection,
    apply_migrations: bool,
    /// WebAuthn relying-party identifier, e.g. `"example.com"`.
    /// Must be an effective domain of `passkey_rp_origin`.
    pub passkey_rp_id: String,
    /// WebAuthn relying-party origin URL, e.g. `"https://example.com"`.
    pub passkey_rp_origin: String,
}

impl ToriiConfig {
    /// Create a Torii config from an existing SeaORM connection.
    ///
    /// This is the standard path — share the connection with the framework's
    /// own database usage. Passkey defaults to `localhost`.
    pub fn from_sea_orm(conn: sea_orm::DatabaseConnection) -> Self {
        Self {
            conn,
            apply_migrations: true,
            passkey_rp_id: "localhost".to_string(),
            passkey_rp_origin: "http://localhost".to_string(),
        }
    }

    /// Test/dev helper: spin up an in-memory SQLite SeaORM connection.
    ///
    /// Uses a shared-cache named in-memory database (`?cache=shared`) so the
    /// database survives for as long as at least one connection holds it open.
    /// When stored in the global `TORII` static, the pool's lifetime extends
    /// across multiple async test runtimes.
    ///
    /// Passkey defaults to `localhost` / `http://localhost`.
    pub async fn sqlite_in_memory() -> Result<Self, FrameworkError> {
        let conn = sea_orm::Database::connect("sqlite:file::memory:?cache=shared")
            .await
            .map_err(|e| FrameworkError::internal(format!("sqlite memory: {e}")))?;
        Ok(Self {
            conn,
            apply_migrations: true,
            passkey_rp_id: "localhost".to_string(),
            passkey_rp_origin: "http://localhost".to_string(),
        })
    }

    /// Control whether Torii runs its schema migrations on first init.
    ///
    /// Defaults to `true`.
    pub fn apply_migrations(mut self, yes: bool) -> Self {
        self.apply_migrations = yes;
        self
    }

    /// Set the WebAuthn relying-party identifier (e.g. `"example.com"`).
    ///
    /// The `rp_id` must be an effective domain of `rp_origin`. Defaults to
    /// `"localhost"`.
    pub fn passkey_rp_id(mut self, rp_id: impl Into<String>) -> Self {
        self.passkey_rp_id = rp_id.into();
        self
    }

    /// Set the WebAuthn relying-party origin URL (e.g. `"https://example.com"`).
    ///
    /// Defaults to `"http://localhost"`.
    pub fn passkey_rp_origin(mut self, rp_origin: impl Into<String>) -> Self {
        self.passkey_rp_origin = rp_origin.into();
        self
    }
}

/// Initialise the global Torii instance.
///
/// Safe to call multiple times — subsequent calls are no-ops. The `OnceLock`
/// ensures only the first caller wins; all others return `Ok(())` immediately.
///
/// # Errors
///
/// Returns [`FrameworkError`] if migrations fail or the connection is invalid.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::torii_integration::{init_torii, ToriiConfig};
///
/// let config = ToriiConfig::sqlite_in_memory().await?;
/// init_torii(config).await?;
/// ```
pub async fn init_torii(config: ToriiConfig) -> Result<(), FrameworkError> {
    if TORII.get().is_some() {
        return Ok(());
    }

    // Initialise WebAuthn before the torii lock so the passkey facade is
    // always ready after init_torii returns.
    passkey::init_webauthn(&config.passkey_rp_id, &config.passkey_rp_origin)?;

    let storage = SeaORMStorage::new(config.conn);

    if config.apply_migrations {
        storage
            .migrate()
            .await
            .map_err(|e| FrameworkError::internal(format!("torii migrate: {e}")))?;
    }

    let provider = Arc::new(storage.into_repository_provider());
    let torii = Torii::new(provider.clone());

    // Store the raw provider so internal code (e.g. passkey) can call
    // find_or_create_by_email without creating a dummy password row.
    let _ = PROVIDER.set(provider);

    // Ignore set() error — another caller may have raced us. Either winner
    // produces an equivalent, fully-initialised instance.
    let _ = TORII.set(torii);
    Ok(())
}

/// Retrieve a reference to the initialised Torii instance.
///
/// # Errors
///
/// Returns [`FrameworkError`] if [`init_torii`] has not been called yet.
pub(crate) fn instance() -> Result<&'static Torii<SeaORMRepositoryProvider>, FrameworkError> {
    TORII
        .get()
        .ok_or_else(|| FrameworkError::internal("Torii not initialised. Call init_torii() first."))
}

