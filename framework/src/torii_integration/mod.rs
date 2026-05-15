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
pub mod oauth;
pub mod passkey;
pub mod password;

use std::sync::{Arc, OnceLock};

use torii::seaorm::SeaORMRepositoryProvider;
use torii::Torii;
use torii_storage_seaorm::SeaORMStorage;

use crate::error::FrameworkError;

// Re-export torii's User and Session so consumers only depend on suprnova::*.
pub use torii::{Session, SessionToken, User, UserId};

/// The single global Torii instance, pinned to the SeaORM repository provider.
static TORII: OnceLock<Torii<SeaORMRepositoryProvider>> = OnceLock::new();

/// Configuration for initialising Torii authentication.
///
/// Create one with [`ToriiConfig::from_sea_orm`] (typical) or
/// [`ToriiConfig::sqlite_in_memory`] (tests/dev).
pub struct ToriiConfig {
    conn: sea_orm::DatabaseConnection,
    apply_migrations: bool,
}

impl ToriiConfig {
    /// Create a Torii config from an existing SeaORM connection.
    ///
    /// This is the standard path — share the connection with the framework's
    /// own database usage.
    pub fn from_sea_orm(conn: sea_orm::DatabaseConnection) -> Self {
        Self {
            conn,
            apply_migrations: true,
        }
    }

    /// Test/dev helper: spin up an in-memory SQLite SeaORM connection.
    ///
    /// Uses a shared-cache named in-memory database (`?cache=shared`) so the
    /// database survives for as long as at least one connection holds it open.
    /// When stored in the global `TORII` static, the pool's lifetime extends
    /// across multiple async test runtimes.
    pub async fn sqlite_in_memory() -> Result<Self, FrameworkError> {
        let conn = sea_orm::Database::connect("sqlite:file::memory:?cache=shared")
            .await
            .map_err(|e| FrameworkError::internal(format!("sqlite memory: {e}")))?;
        Ok(Self {
            conn,
            apply_migrations: true,
        })
    }

    /// Control whether Torii runs its schema migrations on first init.
    ///
    /// Defaults to `true`.
    pub fn apply_migrations(mut self, yes: bool) -> Self {
        self.apply_migrations = yes;
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

    let storage = SeaORMStorage::new(config.conn);

    if config.apply_migrations {
        storage
            .migrate()
            .await
            .map_err(|e| FrameworkError::internal(format!("torii migrate: {e}")))?;
    }

    let provider = Arc::new(storage.into_repository_provider());
    let torii = Torii::new(provider);

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

