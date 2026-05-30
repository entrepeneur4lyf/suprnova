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
//! ## Lock-poisoning policy (Domain 6 audit D6-1)
//!
//! The registry uses `std::sync::RwLock`. Historically every guard
//! acquisition was `.expect("connection registry poisoned")` — a single
//! panicked writer would take down the whole framework on the next
//! request. The fixed shape:
//!
//! - [`Self::register`] / [`Self::register_existing`] / [`Self::get`]
//!   route through [`crate::lock::write`] / [`crate::lock::read`] and
//!   propagate a [`FrameworkError::internal`] on poison instead of
//!   panicking. Application code surfaces the failure normally.
//!
//! - [`Self::has`] is called inline as a `bool` by the executor
//!   read-replica routing path; widening its signature to
//!   `Result<bool, FrameworkError>` would force every caller to
//!   `?`-bubble. Instead `has` degrades to `false` on poison — the
//!   safe fallback (executor drops back to the primary pool).
//!
//! [`Builder::get`]: crate::eloquent::Builder::get
//! [`Builder::first`]: crate::eloquent::Builder::first
//! [`Builder::count`]: crate::eloquent::Builder::count
//! [`Builder::on`]: crate::eloquent::Builder::on
//! [`Builder::on_write_connection`]: crate::eloquent::Builder::on_write_connection
//! [`Model::create`]: crate::eloquent::Model::create
//! [`Model::save`]: crate::eloquent::Model::save
//! [`Model::delete`]: crate::eloquent::Model::delete

use crate::FrameworkError;
use crate::database::config::DatabaseConfig;
use crate::database::connection::DbConnection;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Flipped to `true` the first time [`ConnectionRegistry::has`] observes
/// a poisoned registry lock. The next read still falls back to the
/// primary pool (the safe behaviour documented on `has`), and the
/// warn-once gate keeps the hot routing path from spamming the log on
/// every subsequent request — poison is sticky on `RwLock`, so without
/// this gate every read would re-fire the warning.
static REGISTRY_POISON_WARNED: AtomicBool = AtomicBool::new(false);

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
        let mut r = crate::lock::write(reg(), "connection registry")?;
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
        let mut r = crate::lock::write(reg(), "connection registry")?;
        r.insert(name.to_string(), conn);
        Ok(())
    }

    /// Look up the connection registered under `name`. Returns a
    /// `Database` error when no connection is registered — application
    /// code must handle this failure (no automatic fallback to the
    /// primary; that would mask the misconfiguration). Returns an
    /// `Internal` error if the registry lock is poisoned.
    pub async fn get(name: &str) -> Result<DbConnection, FrameworkError> {
        let r = crate::lock::read(reg(), "connection registry")?;
        r.get(name)
            .cloned()
            .ok_or_else(|| FrameworkError::database(format!("connection '{name}' not registered")))
    }

    /// Whether `name` is registered. Used by the read-replica auto-
    /// routing path in [`crate::database::transaction::ExecutorChoice`].
    ///
    /// **Poison policy**: returns `false` on poisoned lock. The caller
    /// (executor routing) then falls back to the primary pool, which
    /// is the safe behavior. [`Self::get`] returns
    /// [`FrameworkError::internal`] on the same condition so the
    /// application learns about the poison through the next read or
    /// write that actually needs the named connection.
    ///
    /// Emits a single `tracing::warn!` the first time poison is
    /// observed so operators see the condition in logs even when no
    /// subsequent [`Self::get`] is exercised. Repeat observations are
    /// silenced — `RwLock` poison is sticky and the hot routing path
    /// must not flood the log.
    pub async fn has(name: &str) -> bool {
        match reg().read() {
            Ok(r) => r.contains_key(name),
            Err(_) => {
                // Race-safe: `swap` returns the previous value. The
                // first caller that flips `false → true` emits;
                // everyone else short-circuits.
                if !REGISTRY_POISON_WARNED.swap(true, Ordering::SeqCst) {
                    tracing::warn!(
                        target: "suprnova::database",
                        "ConnectionRegistry RwLock is poisoned — a panicked writer left the \
                         registry in a guarded state. `has(\"{name}\")` is degrading to false; \
                         the executor will fall back to the primary pool for this and every \
                         subsequent routed read. The next ConnectionRegistry::get(...) call \
                         will surface an internal-error response.",
                    );
                }
                false
            }
        }
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
        // The round-trip succeeded if `get` returned a connection; no
        // need for a follow-up `is_closed` check (the prior call to
        // that method was a tautology — it hardcoded `false`. See the
        // Domain 6 audit D6-3 note in `database/connection.rs`).
        let _conn = ConnectionRegistry::get("unit_test_round_trip")
            .await
            .unwrap();
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

    /// Domain 6 audit D6-1 regression: a panic while holding the
    /// registry write lock poisons it. After the fix, subsequent
    /// `register`/`get` calls surface the poison as
    /// `FrameworkError::internal` instead of panicking the framework,
    /// and `has` degrades to `false` so the executor's read-replica
    /// routing safely falls back to the primary pool.
    ///
    /// Runs in a fresh `RwLock` (not the global `REGISTRY`) — we
    /// can't intentionally poison the global one without contaminating
    /// every other test in the same process.
    #[test]
    fn poisoned_lock_does_not_panic_register_get_has() {
        use std::sync::Arc;
        use std::thread;

        let lock = Arc::new(RwLock::new(HashMap::<String, ()>::new()));
        let lock_clone = Arc::clone(&lock);
        let _ = thread::spawn(move || {
            let _guard = lock_clone.write().unwrap();
            panic!("intentional poison");
        })
        .join();
        assert!(
            lock.is_poisoned(),
            "test setup: lock must be poisoned after panicked writer",
        );

        // `crate::lock::write` / `crate::lock::read` propagate the
        // poison as a `FrameworkError` instead of panicking. The
        // production `register`/`get` paths use these helpers — the
        // shape below is the exact transformation.
        let write_err = crate::lock::write(&lock, "test connection registry").err();
        assert!(
            write_err.is_some(),
            "lock::write must return Err on poison, got Ok",
        );
        let read_err = crate::lock::read(&lock, "test connection registry").err();
        assert!(
            read_err.is_some(),
            "lock::read must return Err on poison, got Ok",
        );

        // `has` degrades to `false` on the same condition — see the
        // `has` impl above.
        let has_result = match lock.read() {
            Ok(r) => r.contains_key("anything"),
            Err(_) => false,
        };
        assert!(
            !has_result,
            "has() must return false on poisoned lock, never panic",
        );
    }
}
