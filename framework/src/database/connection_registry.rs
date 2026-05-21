//! Phase 10C T12 — named connections + read-write split routing.
//!
//! [`ConnectionRegistry`] holds every named [`DbConnection`] the
//! application has registered through [`crate::DB::register_named`].
//! Lookups go through [`ConnectionRegistry::get`]; routing happens at
//! the executor-dispatch layer in
//! [`crate::database::transaction::ExecutorChoice`].
//!
//! ## Reserved names
//!
//! Two names are reserved by the framework:
//!
//! - `__primary__` — the default pool reachable through
//!   [`crate::DB::connection`]. Cannot be registered into the registry;
//!   the registry rejects the attempt. [`Builder::on_write_connection`]
//!   sets this name on a per-query builder to opt back to the primary
//!   pool when a read replica is otherwise routing reads elsewhere.
//!
//! - `__read_replica__` — the read replica. When registered, every
//!   read-shape terminal method ([`Builder::get`], [`Builder::first`],
//!   [`Builder::count`], etc.) routes through it by default; writes
//!   ([`Model::create`], [`Model::save`], [`Model::delete`]) ignore it
//!   and target the primary. Per-query opt-outs:
//!   [`Builder::on_write_connection`] (one query to primary) or
//!   [`Builder::on(name)`] (one query to an arbitrary named connection).
//!
//! ## Test isolation
//!
//! The registry is process-global. Tests that mutate it should either
//! be annotated `#[serial_test::serial]` or use a unique connection
//! name per test. [`ConnectionRegistry::clear`] wipes every registered
//! name and is called by the [`TestDatabase`](crate::testing::TestDatabase)
//! teardown so the next test in the same process starts with an empty
//! registry.
//!
//! [`Builder::get`]: crate::eloquent::Builder::get
//! [`Builder::first`]: crate::eloquent::Builder::first
//! [`Builder::count`]: crate::eloquent::Builder::count
//! [`Builder::on`]: crate::eloquent::Builder::on
//! [`Builder::on_write_connection`]: crate::eloquent::Builder::on_write_connection
//! [`Model::create`]: crate::eloquent::Model::create
//! [`Model::save`]: crate::eloquent::Model::save
//! [`Model::delete`]: crate::eloquent::Model::delete

use crate::database::config::DatabaseConfig;
use crate::database::connection::DbConnection;
use crate::FrameworkError;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// Process-global registry of named database connections.
///
/// See the [module docs](self) for reserved names + the read-write
/// split contract.
pub struct ConnectionRegistry;

/// The name of the default pool. Reserved — registering anything under
/// this name fails. [`Builder::on_write_connection`] sets it on a
/// builder to opt that query back to the default pool.
///
/// [`Builder::on_write_connection`]: crate::eloquent::Builder::on_write_connection
pub const PRIMARY_CONNECTION_NAME: &str = "__primary__";

/// The name of the read replica. When a connection is registered under
/// this name, every read-shape terminal method routes through it by
/// default; writes ignore it and target the primary.
pub const READ_REPLICA_CONNECTION_NAME: &str = "__read_replica__";

/// Process-global storage. `std::sync::RwLock` (not `tokio::sync`) so
/// the registry can be inspected from sync contexts — most importantly
/// from `Drop` implementations that call [`ConnectionRegistry::clear`]
/// for test isolation. Cloning a [`DbConnection`] is an `Arc::clone`
/// (sync, cheap), so the read path never blocks an async task.
static REGISTRY: OnceLock<RwLock<HashMap<String, DbConnection>>> = OnceLock::new();

fn reg() -> &'static RwLock<HashMap<String, DbConnection>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

impl ConnectionRegistry {
    /// Open a fresh connection pool from `config` and register it
    /// under `name`. Production entry point — called from the
    /// application boot sequence.
    ///
    /// `__primary__` is rejected (it's the default pool reachable via
    /// [`crate::DB::connection`]; this method would shadow that
    /// without changing the routing logic that prefers `connection()`
    /// for `__primary__` lookups).
    pub async fn register(name: &str, config: DatabaseConfig) -> Result<(), FrameworkError> {
        Self::ensure_name_writable(name)?;
        let conn = DbConnection::connect(&config).await?;
        let mut r = reg().write().expect("connection registry poisoned");
        r.insert(name.to_string(), conn);
        Ok(())
    }

    /// Register an already-constructed [`DbConnection`] under `name`.
    /// Useful in tests that have built their own in-memory SQLite
    /// connection (via [`crate::testing::TestDatabase`]) and want it
    /// visible to per-query routing without re-opening it.
    ///
    /// Same reserved-name policy as [`Self::register`].
    pub async fn register_existing(name: &str, conn: DbConnection) -> Result<(), FrameworkError> {
        Self::ensure_name_writable(name)?;
        let mut r = reg().write().expect("connection registry poisoned");
        r.insert(name.to_string(), conn);
        Ok(())
    }

    /// Look up the connection registered under `name`. Returns a
    /// `Database` error when no connection is registered — application
    /// code must handle this failure (no automatic fallback to the
    /// primary; that would mask the misconfiguration).
    pub async fn get(name: &str) -> Result<DbConnection, FrameworkError> {
        let r = reg().read().expect("connection registry poisoned");
        r.get(name).cloned().ok_or_else(|| {
            FrameworkError::database(format!("connection '{name}' not registered"))
        })
    }

    /// Whether `name` is registered. Used by the read-replica auto-
    /// routing path in [`crate::database::transaction::ExecutorChoice`].
    pub async fn has(name: &str) -> bool {
        let r = reg().read().expect("connection registry poisoned");
        r.contains_key(name)
    }

    /// Remove every registered connection. Called from
    /// [`TestContainerGuard`](crate::testing::TestContainerGuard)
    /// teardown so the next test in the same process starts with an
    /// empty registry. Production code does not call this.
    ///
    /// Sync — does not block on the lock for `await`, which is what
    /// lets it run from a `Drop` impl.
    #[doc(hidden)]
    pub fn clear() {
        if let Some(lock) = REGISTRY.get()
            && let Ok(mut r) = lock.write()
        {
            r.clear();
        }
    }

    /// Reject the reserved primary name. Other reserved names like
    /// `__read_replica__` ARE registerable — the framework reads them
    /// back through [`Self::has`] / [`Self::get`].
    fn ensure_name_writable(name: &str) -> Result<(), FrameworkError> {
        if name == PRIMARY_CONNECTION_NAME {
            return Err(FrameworkError::bad_request(format!(
                "connection name '{PRIMARY_CONNECTION_NAME}' is reserved for the default pool; \
                 register the default through DB::init / DB::init_with"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests run `#[serial]` because the registry is process-
    //! global and these tests mutate it. The integration suite at
    //! `framework/tests/database_multiconnection.rs` has the same
    //! constraint; both rely on the [`TestContainerGuard`] drop hook
    //! to wipe the registry between tests but `#[serial]` ensures no
    //! two tests are inside their bodies at the same time.

    use super::*;
    use crate::database::testing::TestDatabase;
    use serial_test::serial;

    #[tokio::test]
    #[serial]
    async fn register_existing_then_get_round_trips() {
        let db = TestDatabase::sqlite_memory().await.unwrap();
        ConnectionRegistry::clear();

        ConnectionRegistry::register_existing("unit_test_round_trip", db.db().clone())
            .await
            .unwrap();
        assert!(ConnectionRegistry::has("unit_test_round_trip").await);
        let conn = ConnectionRegistry::get("unit_test_round_trip").await.unwrap();
        assert!(!conn.is_closed());
    }

    #[tokio::test]
    #[serial]
    async fn registering_primary_is_rejected() {
        let db = TestDatabase::sqlite_memory().await.unwrap();
        ConnectionRegistry::clear();

        let result =
            ConnectionRegistry::register_existing(PRIMARY_CONNECTION_NAME, db.db().clone()).await;
        // DbConnection isn't Debug, so we can't call .unwrap_err()
        // directly. Pattern-match and assert on the message instead.
        match result {
            Ok(()) => panic!("registering __primary__ must fail"),
            Err(err) => {
                let msg = format!("{err}");
                assert!(
                    msg.contains("reserved"),
                    "error must mention 'reserved'; got: {msg}"
                );
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn get_unknown_name_errors() {
        ConnectionRegistry::clear();
        let result = ConnectionRegistry::get("never_registered_unit_test").await;
        match result {
            Ok(_) => panic!("get on unregistered name must fail"),
            Err(err) => {
                let msg = format!("{err}");
                assert!(
                    msg.contains("not registered"),
                    "error must mention 'not registered'; got: {msg}"
                );
            }
        }
    }
}
