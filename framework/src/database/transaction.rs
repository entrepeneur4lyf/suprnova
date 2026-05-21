//! Phase 10C T11 — Transactions: closure form, manual form,
//! savepoints, retry-on-deadlock.
//!
//! Three transaction entry points:
//!
//! - [`DB::transaction`](crate::DB::transaction) — closure form. The
//!   closure runs inside a transaction; commit on `Ok`, rollback on
//!   `Err`. Operations inside the closure pick up the active
//!   transaction automatically via a `tokio::task_local` — callers
//!   don't have to thread a tx handle through every model call.
//!
//! - [`DB::begin_transaction`](crate::DB::begin_transaction) — manual
//!   form. Returns a [`Transaction`] handle the caller commits or
//!   rolls back explicitly. Useful when the transaction's lifetime
//!   spans multiple control-flow branches that don't fit a closure.
//!   Manual mode does NOT install [`CURRENT_TX`]; callers opt every
//!   operation into the transaction with `Builder::with_tx(&tx)` or
//!   the `Model::*_with_tx` shims.
//!
//! - [`DB::transaction_with_attempts`](crate::DB::transaction_with_attempts)
//!   — retry-on-deadlock closure form. Re-runs the closure up to `n`
//!   times when the inner `FrameworkError` looks like a serialization
//!   failure or deadlock (Postgres SQLSTATE `40001` / `40P01`, or any
//!   error containing the case-insensitive substring `"deadlock"`).
//!
//! ## Savepoints
//!
//! Inside the closure, [`Transaction::savepoint`] /
//! [`Transaction::rollback_to`] checkpoint and roll back nested work
//! without aborting the outer transaction. SQLite supports
//! `SAVEPOINT` / `ROLLBACK TO SAVEPOINT` even though it doesn't have
//! row-level locking — the user-visible contract ("commit inner work
//! only if everything succeeded; otherwise restore the snapshot") is
//! the same across all three backends.
//!
//! ## Nested `DB::transaction` is rejected at runtime
//!
//! SeaORM's `DatabaseConnection::begin()` doesn't compose — calling
//! it on a connection that's already holding a transaction starts a
//! brand-new physical transaction that commits / rolls back
//! independently of the outer scope. That's a silent data-integrity
//! footgun, so [`DB::transaction`] checks [`CURRENT_TX`] up front
//! and returns a database error instead of producing the wrong
//! semantics. Use [`Transaction::savepoint`] for nested behaviour.

use crate::database::DB;
use crate::error::FrameworkError;
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use std::sync::Arc;

tokio::task_local! {
    /// Active transaction installed by [`DB::transaction`] /
    /// [`DB::transaction_with_attempts`] for the duration of their
    /// inner closure. Every terminal method on `Builder<M>` and every
    /// CRUD method on `Model` consults this — when `Some(_)`, the SQL
    /// runs through the transaction's connection; otherwise the
    /// global pool from [`DB::connection`] handles it.
    ///
    /// Implementation detail — exposed `pub(crate)` because the
    /// executor-dispatch helpers in `eloquent::builder` and
    /// `eloquent::model` need to read it from outside this module.
    pub(crate) static CURRENT_TX: Option<Arc<DatabaseTransaction>>;
}

/// Handle returned by [`DB::begin_transaction`] and surfaced as
/// `&Transaction` inside the closure form. Owns the active
/// `DatabaseTransaction` until [`Self::commit`] / [`Self::rollback`]
/// consume it.
///
/// Holding a `Transaction` ties up one connection from the pool for
/// the lifetime of the handle. On SQLite (single shared connection)
/// any parallel non-transactional read will block until the
/// transaction completes — load any pre-flight rows BEFORE
/// `DB::begin_transaction()` and scope every dependent write through
/// the returned `tx` handle.
pub struct Transaction {
    pub(crate) inner: Arc<DatabaseTransaction>,
}

/// Cheap shareable view of a [`Transaction`] used to scope a single
/// query through `Builder::with_tx(&tx)` /
/// `Model::*_with_tx(&tx, ...)`. Cloning a `TxHandle` is an
/// `Arc::clone` — every clone points at the same underlying
/// `DatabaseTransaction`.
///
/// `TxHandle` is also the executor-dispatch carrier inside
/// `Builder<M>::tx_override` — when set, it short-circuits the
/// [`CURRENT_TX`] lookup so a builder cloned out of a tx scope can
/// still target the original transaction.
#[derive(Clone)]
pub struct TxHandle {
    pub(crate) inner: Arc<DatabaseTransaction>,
}

impl TxHandle {
    /// Borrow the underlying SeaORM transaction. Internal — exposed
    /// `pub(crate)` so the executor-dispatch helpers in
    /// [`ExecutorChoice`] can reach the same `DatabaseTransaction` the
    /// closure / `Transaction` handle owns. User code goes through
    /// `Builder::with_tx(&tx)` or `Model::*_with_tx(&tx)` instead.
    #[allow(dead_code)] // retained for symmetry; ExecutorChoice reaches `self.inner` directly.
    pub(crate) fn as_conn(&self) -> &DatabaseTransaction {
        &self.inner
    }
}

// ---- ExecutorChoice -----------------------------------------------------

/// Internal dispatch helper. Every terminal method that used to call
/// `DB::connection()?` now calls [`ExecutorChoice::resolve`] (or
/// [`ExecutorChoice::resolve_with_override`] for builders carrying a
/// `tx_override`) and routes the query through the variant arm.
///
/// The three-way precedence is:
///
/// 1. **Builder-level override** — `Builder::with_tx(&tx)` /
///    `Model::*_with_tx(&tx, ...)` set a [`TxHandle`] on the builder.
///    Takes precedence over the task-local because explicit beats
///    ambient.
/// 2. **Ambient `CURRENT_TX`** — installed by [`DB::transaction`] /
///    [`DB::transaction_with_attempts`] for the closure's task scope.
/// 3. **Pool fallback** — `DB::connection()?` returns the global
///    [`DbConnection`](crate::database::DbConnection) singleton.
///
/// The arm-by-arm `match` is verbose but mechanically sound — SeaORM
/// generics on `&C: ConnectionTrait` don't compose into a single
/// `&dyn ConnectionTrait` cleanly because the trait isn't dyn-safe
/// across every helper we touch. Per-method match arms sidestep the
/// dyn-dispatch problem.
#[doc(hidden)]
pub enum ExecutorChoice {
    /// Route through an active transaction's connection (closure form
    /// CURRENT_TX or explicit `with_tx` override).
    Tx(Arc<DatabaseTransaction>),
    /// Route through the global pool from `DB::connection()`.
    Pool(crate::database::DbConnection),
}

impl ExecutorChoice {
    /// Pick the executor for an operation that has no builder-level
    /// override. Consults [`CURRENT_TX`] first, then falls back to
    /// the global pool.
    ///
    /// Doc-hidden internal API. Public visibility is required because
    /// the `#[suprnova::model]` macro emits code in user crates that
    /// references it; user code should not call it directly.
    #[doc(hidden)]
    pub fn resolve() -> Result<Self, FrameworkError> {
        if let Ok(Some(tx)) = CURRENT_TX.try_with(|t| t.clone()) {
            return Ok(ExecutorChoice::Tx(tx));
        }
        Ok(ExecutorChoice::Pool(DB::connection()?))
    }

    /// Pick the executor for an operation that may carry a builder-
    /// level override. The override wins outright when present —
    /// otherwise the behaviour matches [`Self::resolve`].
    #[doc(hidden)]
    pub fn resolve_with_override(
        override_handle: Option<&TxHandle>,
    ) -> Result<Self, FrameworkError> {
        if let Some(h) = override_handle {
            return Ok(ExecutorChoice::Tx(h.inner.clone()));
        }
        Self::resolve()
    }

    /// Build an executor that routes through a specific transaction.
    /// Used by the `Model::*_with_tx` shims, which bypass both the
    /// builder override and the ambient `CURRENT_TX` because the
    /// caller has supplied the tx handle explicitly.
    #[doc(hidden)]
    pub fn from_tx(tx: &Transaction) -> Self {
        ExecutorChoice::Tx(tx.inner.clone())
    }

    /// Get the active SeaORM database backend (Postgres / MySQL /
    /// SQLite). Threaded into the per-backend SQL renderers.
    #[doc(hidden)]
    pub fn backend(&self) -> sea_orm::DbBackend {
        match self {
            ExecutorChoice::Tx(t) => t.get_database_backend(),
            ExecutorChoice::Pool(c) => c.inner().get_database_backend(),
        }
    }

    /// Execute a SeaORM-built `Select<E>` and materialise every
    /// matching row into `E::Model`.
    #[doc(hidden)]
    pub async fn select_all<E>(
        &self,
        q: sea_orm::Select<E>,
    ) -> Result<Vec<E::Model>, sea_orm::DbErr>
    where
        E: sea_orm::EntityTrait,
    {
        match self {
            ExecutorChoice::Tx(t) => q.all(t.as_ref()).await,
            ExecutorChoice::Pool(c) => q.all(c.inner()).await,
        }
    }

    /// Execute a SeaORM-built `Select<E>` and materialise at most one
    /// row into `E::Model`.
    #[doc(hidden)]
    pub async fn select_one<E>(
        &self,
        q: sea_orm::Select<E>,
    ) -> Result<Option<E::Model>, sea_orm::DbErr>
    where
        E: sea_orm::EntityTrait,
    {
        match self {
            ExecutorChoice::Tx(t) => q.one(t.as_ref()).await,
            ExecutorChoice::Pool(c) => q.one(c.inner()).await,
        }
    }

    /// Execute a prepared `Statement` that produces rows.
    #[doc(hidden)]
    pub async fn query_all(
        &self,
        stmt: sea_orm::Statement,
    ) -> Result<Vec<sea_orm::QueryResult>, sea_orm::DbErr> {
        match self {
            ExecutorChoice::Tx(t) => t.query_all(stmt).await,
            ExecutorChoice::Pool(c) => c.inner().query_all(stmt).await,
        }
    }

    /// Execute a prepared `Statement` that produces at most one row.
    #[doc(hidden)]
    pub async fn query_one(
        &self,
        stmt: sea_orm::Statement,
    ) -> Result<Option<sea_orm::QueryResult>, sea_orm::DbErr> {
        match self {
            ExecutorChoice::Tx(t) => t.query_one(stmt).await,
            ExecutorChoice::Pool(c) => c.inner().query_one(stmt).await,
        }
    }

    /// Execute a prepared `Statement` that doesn't produce rows
    /// (INSERT / UPDATE / DELETE / DDL).
    #[doc(hidden)]
    pub async fn run(
        &self,
        stmt: sea_orm::Statement,
    ) -> Result<sea_orm::ExecResult, sea_orm::DbErr> {
        match self {
            ExecutorChoice::Tx(t) => t.execute(stmt).await,
            ExecutorChoice::Pool(c) => c.inner().execute(stmt).await,
        }
    }

    /// Insert an active model. Routes through the active transaction
    /// or the pool depending on the variant.
    #[doc(hidden)]
    pub async fn insert_active<A>(
        &self,
        am: A,
    ) -> Result<<A::Entity as sea_orm::EntityTrait>::Model, sea_orm::DbErr>
    where
        A: sea_orm::ActiveModelTrait + sea_orm::ActiveModelBehavior + Send + 'static,
        <A::Entity as sea_orm::EntityTrait>::Model:
            Send + sea_orm::IntoActiveModel<A>,
    {
        match self {
            ExecutorChoice::Tx(t) => {
                <A as sea_orm::ActiveModelTrait>::insert(am, t.as_ref()).await
            }
            ExecutorChoice::Pool(c) => {
                <A as sea_orm::ActiveModelTrait>::insert(am, c.inner()).await
            }
        }
    }

    /// Update an active model. Routes through the active transaction
    /// or the pool depending on the variant.
    #[doc(hidden)]
    pub async fn update_active<A>(
        &self,
        am: A,
    ) -> Result<<A::Entity as sea_orm::EntityTrait>::Model, sea_orm::DbErr>
    where
        A: sea_orm::ActiveModelTrait + sea_orm::ActiveModelBehavior + Send + 'static,
        <A::Entity as sea_orm::EntityTrait>::Model:
            Send + sea_orm::IntoActiveModel<A>,
    {
        match self {
            ExecutorChoice::Tx(t) => {
                <A as sea_orm::ActiveModelTrait>::update(am, t.as_ref()).await
            }
            ExecutorChoice::Pool(c) => {
                <A as sea_orm::ActiveModelTrait>::update(am, c.inner()).await
            }
        }
    }

    /// Delete an active model. Routes through the active transaction
    /// or the pool depending on the variant.
    #[doc(hidden)]
    pub async fn delete_active<A>(
        &self,
        am: A,
    ) -> Result<sea_orm::DeleteResult, sea_orm::DbErr>
    where
        A: sea_orm::ActiveModelTrait + sea_orm::ActiveModelBehavior + Send + 'static,
    {
        match self {
            ExecutorChoice::Tx(t) => {
                <A as sea_orm::ActiveModelTrait>::delete(am, t.as_ref()).await
            }
            ExecutorChoice::Pool(c) => {
                <A as sea_orm::ActiveModelTrait>::delete(am, c.inner()).await
            }
        }
    }
}

impl Transaction {
    /// Return a clonable handle to this transaction. Pair with
    /// `Builder::with_tx(&tx)` (or the `Model::*_with_tx` variants)
    /// to scope a single operation through the transaction without
    /// installing it as the ambient [`CURRENT_TX`].
    pub fn handle(&self) -> TxHandle {
        TxHandle {
            inner: self.inner.clone(),
        }
    }

    /// Issue `SAVEPOINT <name>` against the active transaction.
    ///
    /// Pair with [`Self::rollback_to`] to drop a block of inner work
    /// while keeping outer changes intact. Works on all three
    /// backends — SQLite's `SAVEPOINT` is fully functional even
    /// though SQLite has no row-level locking.
    ///
    /// The savepoint name is interpolated verbatim into the SQL — do
    /// NOT splice user input into it. Use a static identifier.
    pub async fn savepoint(&self, name: &str) -> Result<(), FrameworkError> {
        let sql = format!("SAVEPOINT {name}");
        self.inner
            .execute_unprepared(&sql)
            .await
            .map(|_| ())
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Issue `ROLLBACK TO SAVEPOINT <name>` against the active
    /// transaction. Drops every change made inside the savepoint
    /// without aborting the outer transaction.
    ///
    /// The savepoint name is interpolated verbatim into the SQL — do
    /// NOT splice user input into it.
    pub async fn rollback_to(&self, name: &str) -> Result<(), FrameworkError> {
        let sql = format!("ROLLBACK TO SAVEPOINT {name}");
        self.inner
            .execute_unprepared(&sql)
            .await
            .map(|_| ())
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Commit the manual transaction returned by
    /// [`DB::begin_transaction`]. Consumes the handle — any
    /// [`TxHandle`] clones stored elsewhere become inert (their
    /// `DatabaseTransaction` is still alive in the `Arc`, but the
    /// underlying connection is no longer in a transactional state).
    ///
    /// Errors if any outstanding [`TxHandle`] clones prevent
    /// `Arc::try_unwrap` from unwrapping the inner transaction —
    /// that's the correct behaviour, because committing while
    /// another part of the program might still write through the
    /// same `TxHandle` would create a race.
    pub async fn commit(self) -> Result<(), FrameworkError> {
        let tx = Arc::try_unwrap(self.inner).map_err(|_| {
            FrameworkError::internal(
                "Transaction::commit: TxHandle clones still alive; \
                 drop them before commit so no further writes can race",
            )
        })?;
        tx.commit()
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Roll back the manual transaction returned by
    /// [`DB::begin_transaction`]. Same `Arc::try_unwrap` constraint
    /// as [`Self::commit`].
    pub async fn rollback(self) -> Result<(), FrameworkError> {
        let tx = Arc::try_unwrap(self.inner).map_err(|_| {
            FrameworkError::internal(
                "Transaction::rollback: TxHandle clones still alive; \
                 drop them before rollback so no further writes can race",
            )
        })?;
        tx.rollback()
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))
    }
}

impl DB {
    /// Run `f` inside a database transaction. The closure receives a
    /// `&Transaction` it can use to issue savepoints; operations on
    /// `Builder<M>` / `Model` inside the closure pick up the active
    /// transaction automatically via the [`CURRENT_TX`] task-local.
    ///
    /// - Closure returns `Ok` → commit. Result propagated.
    /// - Closure returns `Err` → rollback. Original error returned.
    ///
    /// Nested `DB::transaction` calls are rejected with a database
    /// error — SeaORM's `begin()` doesn't compose. Use
    /// [`Transaction::savepoint`] for nested-rollback behaviour.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// DB::transaction(|_tx| {
    ///     Box::pin(async move {
    ///         let mut alice = User::query().filter("name", "alice").first_or_fail().await?;
    ///         alice.balance -= 30;
    ///         alice.save().await?;
    ///
    ///         let mut bob = User::query().filter("name", "bob").first_or_fail().await?;
    ///         bob.balance += 30;
    ///         bob.save().await?;
    ///         Ok::<(), FrameworkError>(())
    ///     })
    /// }).await?;
    /// ```
    ///
    /// The `Box::pin(async move { ... })` shape is required because
    /// the closure's return type is `Pin<Box<dyn Future + 'b>>` —
    /// the HRTB lifetime lets the future borrow `&tx` across `.await`
    /// points (so `tx.savepoint(...)` calls work).
    pub async fn transaction<F, T>(f: F) -> Result<T, FrameworkError>
    where
        // HRTB: the closure must accept a borrow of `Transaction`
        // tied to a fresh lifetime `'b` and return a boxed future
        // that captures that borrow. Mirrors SeaORM's
        // `TransactionTrait::transaction` shape, which is the only
        // signature Rust accepts when the future actually USES the
        // `&Transaction` across `.await` points (e.g. calling
        // `tx.savepoint(...)`).
        F: for<'b> FnOnce(
            &'b Transaction,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, FrameworkError>> + Send + 'b>,
        >,
        T: Send,
    {
        // Reject nested calls before doing any work. Without this
        // guard, `conn.inner().begin()` below would start a brand-new
        // top-level transaction on a pooled connection that's
        // independent of the outer scope — silently corrupting the
        // composition semantics callers expect.
        let nested = CURRENT_TX
            .try_with(|t| t.is_some())
            .unwrap_or(false);
        if nested {
            return Err(FrameworkError::database(
                "nested DB::transaction is not supported; use tx.savepoint(name) for nested rollback",
            ));
        }

        let conn = DB::connection()?;
        let tx = conn
            .inner()
            .begin()
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let tx_arc = Arc::new(tx);

        let transaction = Transaction {
            inner: tx_arc.clone(),
        };

        let result = CURRENT_TX
            .scope(Some(tx_arc.clone()), f(&transaction))
            .await;

        // Drop the wrapper BEFORE calling `Arc::try_unwrap`. The
        // `transaction` binding holds the second `Arc` clone (the
        // first is `tx_arc`); without this explicit drop the unwrap
        // always fails with refcount==2 and we'd never commit. The
        // task-local clone is released automatically when
        // `CURRENT_TX::scope` returns.
        drop(transaction);

        match result {
            Ok(value) => {
                let tx = Arc::try_unwrap(tx_arc).map_err(|_| {
                    FrameworkError::internal(
                        "DB::transaction: TxHandle clones outlived the closure; \
                         drop them before the closure returns Ok so commit can proceed",
                    )
                })?;
                tx.commit()
                    .await
                    .map_err(|e| FrameworkError::database(e.to_string()))?;
                Ok(value)
            }
            Err(e) => {
                // Best-effort rollback: if the user stashed a
                // `TxHandle` clone outside the closure we can't
                // unwrap, so we drop the `Arc` and let SeaORM's
                // `DatabaseTransaction::drop` rollback when the last
                // reference goes away. Either way the inner closure
                // error is surfaced to the caller.
                if let Ok(tx) = Arc::try_unwrap(tx_arc) {
                    let _ = tx.rollback().await;
                }
                Err(e)
            }
        }
    }

    /// Open a manual transaction. The caller is responsible for
    /// calling [`Transaction::commit`] or [`Transaction::rollback`];
    /// if the handle is dropped the underlying SeaORM
    /// `DatabaseTransaction::drop` rolls back automatically.
    ///
    /// Manual mode does NOT install [`CURRENT_TX`]. Scope individual
    /// operations through the transaction with `Builder::with_tx(&tx)`
    /// or the `Model::*_with_tx(&tx, ...)` shims.
    ///
    /// Holding a `Transaction` pins one pool connection for its
    /// entire lifetime. Pre-load any rows you need to read BEFORE
    /// calling `begin_transaction`, especially on SQLite (where the
    /// single shared connection is checked out for the tx duration).
    pub async fn begin_transaction() -> Result<Transaction, FrameworkError> {
        let conn = DB::connection()?;
        let tx = conn
            .inner()
            .begin()
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(Transaction {
            inner: Arc::new(tx),
        })
    }

    /// Run `f` inside a transaction, retrying up to `attempts` times
    /// when the inner `FrameworkError` looks like a deadlock or
    /// serialization failure.
    ///
    /// The closure body runs from scratch on every attempt — capture
    /// owned state (or `Arc`s) rather than `&mut` references so the
    /// retry path is well-defined.
    ///
    /// Detection is by Display-string substring against the inner
    /// error:
    ///
    /// - Postgres SQLSTATE `40001` (serialization_failure)
    /// - Postgres SQLSTATE `40P01` (deadlock_detected)
    /// - Case-insensitive `"deadlock"` substring (covers MySQL
    ///   `Deadlock found when trying to get lock` and any user-
    ///   surfaced deadlock string)
    ///
    /// On the final attempt the error propagates unchanged.
    pub async fn transaction_with_attempts<F, T>(
        attempts: u32,
        mut f: F,
    ) -> Result<T, FrameworkError>
    where
        // HRTB matching `transaction` — the closure must accept a
        // freshly-borrowed `&Transaction` per attempt and return a
        // boxed future that borrows it. The `FnMut` bound lets the
        // closure capture state (e.g. an `Arc<AtomicU32>` retry
        // counter) and mutate it across attempts.
        F: for<'b> FnMut(
                &'b Transaction,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<T, FrameworkError>> + Send + 'b>,
            > + Send,
        T: Send,
    {
        if attempts == 0 {
            return Err(FrameworkError::database(
                "transaction_with_attempts called with attempts = 0",
            ));
        }
        for attempt in 1..=attempts {
            match DB::transaction(|tx| f(tx)).await {
                Ok(v) => return Ok(v),
                Err(e) if is_deadlock(&e) && attempt < attempts => {
                    tracing::warn!(
                        target: "suprnova::eloquent::tx",
                        attempt,
                        max_attempts = attempts,
                        error = %e,
                        "transaction deadlocked, retrying"
                    );
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        // unreachable — the loop either returns `Ok(_)` or the final
        // `Err(_)` branch above. Kept as a hardened fallthrough.
        Err(FrameworkError::internal(
            "transaction_with_attempts: loop exited without returning",
        ))
    }
}

/// Whether `e`'s Display matches the deadlock / serialization-failure
/// pattern. Used by [`DB::transaction_with_attempts`] to decide
/// whether to retry.
fn is_deadlock(e: &FrameworkError) -> bool {
    let msg = format!("{e}");
    msg.contains("40001")
        || msg.contains("40P01")
        || msg.to_lowercase().contains("deadlock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deadlock_matches_postgres_sqlstates() {
        assert!(is_deadlock(&FrameworkError::database(
            "ERROR: could not serialize access (SQLSTATE 40001)"
        )));
        assert!(is_deadlock(&FrameworkError::database(
            "ERROR: deadlock detected (SQLSTATE 40P01)"
        )));
    }

    #[test]
    fn is_deadlock_matches_case_insensitive_deadlock_substring() {
        assert!(is_deadlock(&FrameworkError::database(
            "Deadlock found when trying to get lock"
        )));
        assert!(is_deadlock(&FrameworkError::database("simulated deadlock")));
        assert!(is_deadlock(&FrameworkError::database("DEADLOCK!")));
    }

    #[test]
    fn is_deadlock_rejects_unrelated_errors() {
        assert!(!is_deadlock(&FrameworkError::database(
            "ERROR: relation \"users\" does not exist"
        )));
        assert!(!is_deadlock(&FrameworkError::database("connection refused")));
        assert!(!is_deadlock(&FrameworkError::internal("oops")));
    }
}
