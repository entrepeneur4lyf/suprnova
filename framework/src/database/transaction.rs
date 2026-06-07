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
//!   Manual mode does NOT install `CURRENT_TX`; callers opt every
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
//! footgun, so [`DB::transaction`] checks `CURRENT_TX` up front
//! and returns a database error instead of producing the wrong
//! semantics. Use [`Transaction::savepoint`] for nested behaviour.

use crate::database::DB;
use crate::error::FrameworkError;
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};
use std::sync::Arc;

/// Internal state shared by [`Transaction`], [`TxHandle`], and the
/// task-local [`CURRENT_TX`]. Pairs the SeaORM transaction handle with
/// the logical connection name the transaction was opened against, so
/// every `QueryExecuted` / `TransactionBeginning` / `TransactionCommitted`
/// / `TransactionRolledBack` event carries the actual connection name
/// rather than a hard-coded sentinel.
///
/// `Arc<str>` not `String` for the name — every clone of a `TxHandle`
/// duplicates the pair, and the connection name is small + immutable
/// for the transaction's lifetime.
pub(crate) struct TxState {
    pub(crate) tx: Arc<DatabaseTransaction>,
    pub(crate) connection_name: Arc<str>,
}

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
    pub(crate) static CURRENT_TX: Option<Arc<TxState>>;
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
    pub(crate) connection_name: Arc<str>,
}

/// Cheap shareable view of a [`Transaction`] used to scope a single
/// query through `Builder::with_tx(&tx)` /
/// `Model::*_with_tx(&tx, ...)`. Cloning a `TxHandle` is an
/// `Arc::clone` — every clone points at the same underlying
/// `DatabaseTransaction`.
///
/// `TxHandle` is also the executor-dispatch carrier inside
/// `Builder<M>::tx_override` — when set, it short-circuits the
/// `CURRENT_TX` lookup so a builder cloned out of a tx scope can
/// still target the original transaction.
#[derive(Clone)]
pub struct TxHandle {
    pub(crate) inner: Arc<DatabaseTransaction>,
    pub(crate) connection_name: Arc<str>,
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
    /// CURRENT_TX or explicit `with_tx` override). Second field is the
    /// logical connection name the transaction was opened against —
    /// threaded into every `QueryExecuted` / transaction-lifecycle
    /// event so observers see the real connection.
    Tx(Arc<DatabaseTransaction>, Arc<str>),
    /// Route through a pool. Second field is the logical name of the
    /// pool the executor resolved — `__primary__` for the default,
    /// `__read_replica__` for the auto-routed replica, or whatever
    /// name was passed to [`Builder::on`](crate::eloquent::Builder::on)
    /// / `#[model(connection = "...")]` / [`DB::register_named`].
    Pool(crate::database::DbConnection, Arc<str>),
}

impl ExecutorChoice {
    /// The logical connection name this executor is bound to. Threaded
    /// into [`QueryExecuted::connection_name`](crate::database::events::QueryExecuted::connection_name)
    /// from the instrumentation helpers.
    #[doc(hidden)]
    pub fn connection_name(&self) -> &str {
        match self {
            ExecutorChoice::Tx(_, name) => name,
            ExecutorChoice::Pool(_, name) => name,
        }
    }

    /// Pick the executor for an operation that has no builder-level
    /// override. Consults `CURRENT_TX` first, then falls back to
    /// the global pool.
    ///
    /// Doc-hidden internal API. Public visibility is required because
    /// the `#[suprnova::model]` macro emits code in user crates that
    /// references it; user code should not call it directly.
    #[doc(hidden)]
    pub fn resolve() -> Result<Self, FrameworkError> {
        if let Ok(Some(state)) = CURRENT_TX.try_with(|t| t.clone()) {
            return Ok(ExecutorChoice::Tx(
                state.tx.clone(),
                state.connection_name.clone(),
            ));
        }
        Ok(ExecutorChoice::Pool(
            DB::connection()?,
            crate::database::PRIMARY_CONNECTION_NAME.into(),
        ))
    }

    /// Pick the executor for an operation that may carry a builder-
    /// level override. The override wins outright when present —
    /// otherwise the behaviour matches [`Self::resolve`].
    #[doc(hidden)]
    pub fn resolve_with_override(
        override_handle: Option<&TxHandle>,
    ) -> Result<Self, FrameworkError> {
        if let Some(h) = override_handle {
            return Ok(ExecutorChoice::Tx(
                h.inner.clone(),
                h.connection_name.clone(),
            ));
        }
        Self::resolve()
    }

    /// Phase 10C T12 — pick the executor for a READ-shape operation.
    /// Five-step precedence:
    ///
    /// 1. **Builder-level transaction override** (`Builder::with_tx`).
    ///    Explicit beats every other consideration.
    /// 2. **Ambient `CURRENT_TX`** installed by [`DB::transaction`] /
    ///    [`DB::transaction_with_attempts`]. Inside a closure-form
    ///    transaction every read uses the tx connection — `on(name)`
    ///    routing is silently ignored.
    /// 3. **Per-builder `connection_override`** (`Builder::on(name)`).
    ///    The `__primary__` sentinel short-circuits to
    ///    [`DB::connection`] without consulting the registry.
    /// 4. **Per-model default** (`#[model(connection = "...")]`).
    /// 5. **`__read_replica__`** if registered.
    /// 6. **Default pool** (`DB::connection`).
    ///
    /// Step 1 fires when the closure form's task-local is `Some(_)`;
    /// step 2 is the same lookup but with a builder-attached
    /// [`TxHandle`]. Steps 3-6 are the new T12 routing chain.
    #[doc(hidden)]
    pub async fn resolve_read(
        tx_override: Option<&TxHandle>,
        connection_override: Option<&str>,
        model_default_conn: Option<&'static str>,
    ) -> Result<Self, FrameworkError> {
        // Step 1: explicit builder-level tx override.
        if let Some(h) = tx_override {
            return Ok(ExecutorChoice::Tx(
                h.inner.clone(),
                h.connection_name.clone(),
            ));
        }
        // Step 2: ambient closure-form transaction.
        if let Ok(Some(state)) = CURRENT_TX.try_with(|t| t.clone()) {
            return Ok(ExecutorChoice::Tx(
                state.tx.clone(),
                state.connection_name.clone(),
            ));
        }
        // Step 3: per-builder connection override.
        if let Some(name) = connection_override {
            if name == crate::database::PRIMARY_CONNECTION_NAME {
                return Ok(ExecutorChoice::Pool(
                    DB::connection()?,
                    crate::database::PRIMARY_CONNECTION_NAME.into(),
                ));
            }
            return Ok(ExecutorChoice::Pool(DB::named(name).await?, name.into()));
        }
        // Step 4: per-model default connection.
        if let Some(name) = model_default_conn {
            if name == crate::database::PRIMARY_CONNECTION_NAME {
                return Ok(ExecutorChoice::Pool(
                    DB::connection()?,
                    crate::database::PRIMARY_CONNECTION_NAME.into(),
                ));
            }
            return Ok(ExecutorChoice::Pool(DB::named(name).await?, name.into()));
        }
        // Step 5: read replica if registered.
        if crate::database::ConnectionRegistry::has(crate::database::READ_REPLICA_CONNECTION_NAME)
            .await
        {
            return Ok(ExecutorChoice::Pool(
                DB::named(crate::database::READ_REPLICA_CONNECTION_NAME).await?,
                crate::database::READ_REPLICA_CONNECTION_NAME.into(),
            ));
        }
        // Step 6: default pool.
        Ok(ExecutorChoice::Pool(
            DB::connection()?,
            crate::database::PRIMARY_CONNECTION_NAME.into(),
        ))
    }

    /// Phase 10C T12 — pick the executor for a WRITE-shape operation
    /// (`Model::create`, `Model::save`, `Model::update`, `Model::delete`,
    /// `DbTableBuilder::insert/update/delete`).
    ///
    /// Same precedence as [`Self::resolve_read`] EXCEPT step 5 is
    /// skipped — writes never auto-route to `__read_replica__`. If the
    /// caller wants a write against a non-primary connection they must
    /// chain `Builder::on(name)` (step 3) or tag the model with
    /// `#[model(connection = "...")]` (step 4) explicitly.
    #[doc(hidden)]
    pub async fn resolve_write(
        tx_override: Option<&TxHandle>,
        connection_override: Option<&str>,
        model_default_conn: Option<&'static str>,
    ) -> Result<Self, FrameworkError> {
        if let Some(h) = tx_override {
            return Ok(ExecutorChoice::Tx(
                h.inner.clone(),
                h.connection_name.clone(),
            ));
        }
        if let Ok(Some(state)) = CURRENT_TX.try_with(|t| t.clone()) {
            return Ok(ExecutorChoice::Tx(
                state.tx.clone(),
                state.connection_name.clone(),
            ));
        }
        if let Some(name) = connection_override {
            if name == crate::database::PRIMARY_CONNECTION_NAME {
                return Ok(ExecutorChoice::Pool(
                    DB::connection()?,
                    crate::database::PRIMARY_CONNECTION_NAME.into(),
                ));
            }
            return Ok(ExecutorChoice::Pool(DB::named(name).await?, name.into()));
        }
        if let Some(name) = model_default_conn {
            if name == crate::database::PRIMARY_CONNECTION_NAME {
                return Ok(ExecutorChoice::Pool(
                    DB::connection()?,
                    crate::database::PRIMARY_CONNECTION_NAME.into(),
                ));
            }
            return Ok(ExecutorChoice::Pool(DB::named(name).await?, name.into()));
        }
        // No read-replica auto-routing on writes.
        Ok(ExecutorChoice::Pool(
            DB::connection()?,
            crate::database::PRIMARY_CONNECTION_NAME.into(),
        ))
    }

    /// Build an executor that routes through a specific transaction.
    /// Used by the `Model::*_with_tx` shims, which bypass both the
    /// builder override and the ambient `CURRENT_TX` because the
    /// caller has supplied the tx handle explicitly.
    #[doc(hidden)]
    pub fn from_tx(tx: &Transaction) -> Self {
        ExecutorChoice::Tx(tx.inner.clone(), tx.connection_name.clone())
    }

    /// Get the active SeaORM database backend (Postgres / MySQL /
    /// SQLite). Threaded into the per-backend SQL renderers.
    #[doc(hidden)]
    pub fn backend(&self) -> sea_orm::DbBackend {
        match self {
            ExecutorChoice::Tx(t, _) => t.get_database_backend(),
            ExecutorChoice::Pool(c, _) => c.inner().get_database_backend(),
        }
    }

    /// Execute a SeaORM-built `Select<E>` and materialise every
    /// matching row into `E::Model`. Emits
    /// [`QueryExecuted`](crate::database::events::QueryExecuted) when
    /// observation is active.
    #[doc(hidden)]
    pub async fn select_all<E>(
        &self,
        q: sea_orm::Select<E>,
    ) -> Result<Vec<E::Model>, sea_orm::DbErr>
    where
        E: sea_orm::EntityTrait,
    {
        if super::events::is_dispatching() || !super::events::query_observation_active() {
            return match self {
                ExecutorChoice::Tx(t, _) => q.all(t.as_ref()).await,
                ExecutorChoice::Pool(c, _) => q.all(c.inner()).await,
            };
        }
        let stmt = sea_orm::QueryTrait::build(&q, self.backend());
        let (sql, bindings) = (stmt.sql.clone(), stmt_bindings_strings(&stmt));
        let conn_name = self.connection_name().to_string();
        let start = std::time::Instant::now();
        let res = match self {
            ExecutorChoice::Tx(t, _) => q.all(t.as_ref()).await,
            ExecutorChoice::Pool(c, _) => q.all(c.inner()).await,
        };
        let elapsed = start.elapsed();
        finish_query_event(
            sql,
            bindings,
            elapsed,
            super::events::ReadWriteType::Read,
            conn_name,
            &res,
        )
        .await;
        res
    }

    /// Execute a SeaORM-built `Select<E>` and materialise at most one
    /// row into `E::Model`. See [`Self::select_all`] for the
    /// observability contract.
    #[doc(hidden)]
    pub async fn select_one<E>(
        &self,
        q: sea_orm::Select<E>,
    ) -> Result<Option<E::Model>, sea_orm::DbErr>
    where
        E: sea_orm::EntityTrait,
    {
        if super::events::is_dispatching() || !super::events::query_observation_active() {
            return match self {
                ExecutorChoice::Tx(t, _) => q.one(t.as_ref()).await,
                ExecutorChoice::Pool(c, _) => q.one(c.inner()).await,
            };
        }
        let stmt = sea_orm::QueryTrait::build(&q, self.backend());
        let (sql, bindings) = (stmt.sql.clone(), stmt_bindings_strings(&stmt));
        let conn_name = self.connection_name().to_string();
        let start = std::time::Instant::now();
        let res = match self {
            ExecutorChoice::Tx(t, _) => q.one(t.as_ref()).await,
            ExecutorChoice::Pool(c, _) => q.one(c.inner()).await,
        };
        let elapsed = start.elapsed();
        finish_query_event(
            sql,
            bindings,
            elapsed,
            super::events::ReadWriteType::Read,
            conn_name,
            &res,
        )
        .await;
        res
    }

    /// Execute a SeaORM-built `Select<E>` as a `COUNT(*)` and return the
    /// total matching row count. See [`Self::select_all`] for the
    /// observability contract.
    #[doc(hidden)]
    pub async fn select_count<E>(&self, q: sea_orm::Select<E>) -> Result<u64, sea_orm::DbErr>
    where
        E: sea_orm::EntityTrait,
        E::Model: Send + Sync,
    {
        use sea_orm::PaginatorTrait;
        if super::events::is_dispatching() || !super::events::query_observation_active() {
            return match self {
                ExecutorChoice::Tx(t, _) => q.count(t.as_ref()).await,
                ExecutorChoice::Pool(c, _) => q.count(c.inner()).await,
            };
        }
        let stmt = sea_orm::QueryTrait::build(&q, self.backend());
        let (sql, bindings) = (stmt.sql.clone(), stmt_bindings_strings(&stmt));
        let conn_name = self.connection_name().to_string();
        let start = std::time::Instant::now();
        let res = match self {
            ExecutorChoice::Tx(t, _) => q.count(t.as_ref()).await,
            ExecutorChoice::Pool(c, _) => q.count(c.inner()).await,
        };
        let elapsed = start.elapsed();
        finish_query_event(
            sql,
            bindings,
            elapsed,
            super::events::ReadWriteType::Read,
            conn_name,
            &res,
        )
        .await;
        res
    }

    /// Execute a prepared `Statement` that produces rows.
    ///
    /// Emits [`QueryExecuted`](crate::database::events::QueryExecuted)
    /// to every registered listener — `DB::listen` callback, dispatcher
    /// listener, and the in-memory query log. Re-entrancy guarded:
    /// a listener that re-queries does not re-fire QueryExecuted.
    #[doc(hidden)]
    pub async fn query_all(
        &self,
        stmt: sea_orm::Statement,
    ) -> Result<Vec<sea_orm::QueryResult>, sea_orm::DbErr> {
        self.run_instrumented(stmt, Some(super::events::ReadWriteType::Read), |s, e| {
            Box::pin(async move {
                match e {
                    ExecutorChoice::Tx(t, _) => t.query_all(s).await,
                    ExecutorChoice::Pool(c, _) => c.inner().query_all(s).await,
                }
            })
        })
        .await
    }

    /// Execute a prepared `Statement` that produces at most one row.
    /// See [`Self::query_all`] for the observability contract.
    #[doc(hidden)]
    pub async fn query_one(
        &self,
        stmt: sea_orm::Statement,
    ) -> Result<Option<sea_orm::QueryResult>, sea_orm::DbErr> {
        self.run_instrumented(stmt, Some(super::events::ReadWriteType::Read), |s, e| {
            Box::pin(async move {
                match e {
                    ExecutorChoice::Tx(t, _) => t.query_one(s).await,
                    ExecutorChoice::Pool(c, _) => c.inner().query_one(s).await,
                }
            })
        })
        .await
    }

    /// Execute a prepared `Statement` that doesn't produce rows
    /// (INSERT / UPDATE / DELETE / DDL). See [`Self::query_all`] for
    /// the observability contract.
    #[doc(hidden)]
    pub async fn run(
        &self,
        stmt: sea_orm::Statement,
    ) -> Result<sea_orm::ExecResult, sea_orm::DbErr> {
        self.run_instrumented(stmt, Some(super::events::ReadWriteType::Write), |s, e| {
            Box::pin(async move {
                match e {
                    ExecutorChoice::Tx(t, _) => t.execute(s).await,
                    ExecutorChoice::Pool(c, _) => c.inner().execute(s).await,
                }
            })
        })
        .await
    }

    /// Generic helper that runs a Statement and emits `QueryExecuted`
    /// to every registered observer. Shared by `query_all`, `query_one`,
    /// and `run` so the dispatch shape is in exactly one place.
    ///
    /// Listener errors are observational: the closure's result is
    /// returned to the caller verbatim regardless of listener outcome.
    /// Re-entrancy is guarded — listeners that themselves run queries
    /// will not re-fire QueryExecuted.
    async fn run_instrumented<F, T>(
        &self,
        stmt: sea_orm::Statement,
        rw: Option<super::events::ReadWriteType>,
        f: F,
    ) -> Result<T, sea_orm::DbErr>
    where
        F: for<'a> FnOnce(
            sea_orm::Statement,
            &'a Self,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, sea_orm::DbErr>> + Send + 'a>,
        >,
    {
        // Fast path: if no observer is active OR we're already inside
        // a listener dispatch, skip the SQL/binding capture entirely.
        if super::events::is_dispatching() || !super::events::query_observation_active() {
            return f(stmt, self).await;
        }
        // Capture SQL/bindings BEFORE the call (the Statement is moved
        // into `f`). Cloning here is the cost of observability — gated
        // by the active-observer check above.
        let sql = stmt.sql.clone();
        let bindings: Vec<String> = stmt
            .values
            .as_ref()
            .map(|v| v.0.iter().map(|val| format!("{val:?}")).collect())
            .unwrap_or_default();
        let conn_name = self.connection_name().to_string();
        let start = std::time::Instant::now();
        let res = f(stmt, self).await;
        let elapsed = start.elapsed();
        let result_for_event: Result<(), String> = match &res {
            Ok(_) => Ok(()),
            Err(e) => Err(e.to_string()),
        };
        let event = super::events::QueryExecuted {
            sql,
            bindings,
            time: elapsed,
            connection_name: conn_name,
            read_write_type: rw,
            result: result_for_event,
        };
        emit_query_executed(event).await;
        res
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
        <A::Entity as sea_orm::EntityTrait>::Model: Send + sea_orm::IntoActiveModel<A>,
    {
        match self {
            ExecutorChoice::Tx(t, _) => {
                <A as sea_orm::ActiveModelTrait>::insert(am, t.as_ref()).await
            }
            ExecutorChoice::Pool(c, _) => {
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
        <A::Entity as sea_orm::EntityTrait>::Model: Send + sea_orm::IntoActiveModel<A>,
    {
        match self {
            ExecutorChoice::Tx(t, _) => {
                <A as sea_orm::ActiveModelTrait>::update(am, t.as_ref()).await
            }
            ExecutorChoice::Pool(c, _) => {
                <A as sea_orm::ActiveModelTrait>::update(am, c.inner()).await
            }
        }
    }

    /// Delete an active model. Routes through the active transaction
    /// or the pool depending on the variant.
    #[doc(hidden)]
    pub async fn delete_active<A>(&self, am: A) -> Result<sea_orm::DeleteResult, sea_orm::DbErr>
    where
        A: sea_orm::ActiveModelTrait + sea_orm::ActiveModelBehavior + Send + 'static,
    {
        match self {
            ExecutorChoice::Tx(t, _) => {
                <A as sea_orm::ActiveModelTrait>::delete(am, t.as_ref()).await
            }
            ExecutorChoice::Pool(c, _) => {
                <A as sea_orm::ActiveModelTrait>::delete(am, c.inner()).await
            }
        }
    }
}

impl Transaction {
    /// Return a clonable handle to this transaction. Pair with
    /// `Builder::with_tx(&tx)` (or the `Model::*_with_tx` variants)
    /// to scope a single operation through the transaction without
    /// installing it as the ambient `CURRENT_TX`.
    pub fn handle(&self) -> TxHandle {
        TxHandle {
            inner: self.inner.clone(),
            connection_name: self.connection_name.clone(),
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
    ///
    /// Fires [`TransactionCommitted`](super::events::TransactionCommitted)
    /// after a successful commit.
    pub async fn commit(self) -> Result<(), FrameworkError> {
        let conn_name = self.connection_name.to_string();
        let tx = Arc::try_unwrap(self.inner).map_err(|_| {
            FrameworkError::internal(
                "Transaction::commit: TxHandle clones still alive; \
                 drop them before commit so no further writes can race",
            )
        })?;
        tx.commit()
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        emit_tx_event(super::events::TransactionCommitted {
            connection_name: conn_name,
        })
        .await;
        Ok(())
    }

    /// Roll back the manual transaction returned by
    /// [`DB::begin_transaction`]. Same `Arc::try_unwrap` constraint
    /// as [`Self::commit`].
    ///
    /// Fires [`TransactionRolledBack`](super::events::TransactionRolledBack)
    /// after a successful rollback.
    pub async fn rollback(self) -> Result<(), FrameworkError> {
        let conn_name = self.connection_name.to_string();
        let tx = Arc::try_unwrap(self.inner).map_err(|_| {
            FrameworkError::internal(
                "Transaction::rollback: TxHandle clones still alive; \
                 drop them before rollback so no further writes can race",
            )
        })?;
        tx.rollback()
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        emit_tx_event(super::events::TransactionRolledBack {
            connection_name: conn_name,
        })
        .await;
        Ok(())
    }
}

impl DB {
    /// Run `f` inside a database transaction. The closure receives a
    /// `&Transaction` it can use to issue savepoints; operations on
    /// `Builder<M>` / `Model` inside the closure pick up the active
    /// transaction automatically via the `CURRENT_TX` task-local.
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
        let nested = CURRENT_TX.try_with(|t| t.is_some()).unwrap_or(false);
        if nested {
            return Err(FrameworkError::database(
                "nested DB::transaction is not supported; use tx.savepoint(name) for nested rollback",
            ));
        }

        let conn = DB::connection()?;
        // DB::transaction always opens against the default pool today
        // (no `transaction_on(name)` surface yet); when that lands, the
        // `conn_name` Arc<str> is the only thing that needs to grow.
        let conn_name: Arc<str> = super::PRIMARY_CONNECTION_NAME.into();
        let tx = conn
            .inner()
            .begin()
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        // BEGIN succeeded — fire TransactionBeginning before the
        // closure runs so listeners observe the open tx.
        emit_tx_event(super::events::TransactionBeginning {
            connection_name: conn_name.to_string(),
        })
        .await;
        let tx_arc = Arc::new(tx);

        let transaction = Transaction {
            inner: tx_arc.clone(),
            connection_name: conn_name.clone(),
        };

        let tx_state = Arc::new(TxState {
            tx: tx_arc.clone(),
            connection_name: conn_name.clone(),
        });

        let result = CURRENT_TX.scope(Some(tx_state), f(&transaction)).await;

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
                emit_tx_event(super::events::TransactionCommitted {
                    connection_name: conn_name.to_string(),
                })
                .await;
                Ok(value)
            }
            Err(e) => {
                // Try to roll back immediately. If TxHandle clones
                // were leaked past the closure boundary,
                // `Arc::try_unwrap` returns the `Arc` back — drop it
                // here so OUR reference goes away, log loudly, and
                // surface the original closure error. SeaORM's
                // `DatabaseTransaction::drop` rolls back when the
                // LAST reference drops; until the leaked clones go
                // away the transaction is in a zombie state
                // (queries via the leaked handle still run against
                // an open tx). Audit HIGH `database` #3 — escalate
                // the diagnostic so this can't disappear silently.
                match Arc::try_unwrap(tx_arc) {
                    Ok(tx) => {
                        if let Err(rb_err) = tx.rollback().await {
                            tracing::warn!(
                                error = %rb_err,
                                "Transaction rollback failed after closure error; \
                                 the original closure error is still surfaced to \
                                 the caller. Common cause: connection lost between \
                                 BEGIN and the failing query.",
                            );
                        } else {
                            emit_tx_event(super::events::TransactionRolledBack {
                                connection_name: conn_name.to_string(),
                            })
                            .await;
                        }
                    }
                    Err(arc) => {
                        // Leaked clones — count them before our Arc
                        // drops so the operator sees the size of the
                        // leak.
                        let strong_count = Arc::strong_count(&arc);
                        let leaked = strong_count.saturating_sub(1);
                        drop(arc); // release OUR ref; leaked refs still keep the tx alive
                        tracing::error!(
                            leaked_handles = leaked,
                            closure_error = %e,
                            "DB::transaction: closure returned Err but TxHandle clones \
                             leaked past the closure boundary. The transaction is in \
                             ZOMBIE STATE — pending rollback until ALL leaked handles \
                             drop. Queries via the leaked handles continue to run \
                             against the still-open transaction. Drop them before the \
                             closure returns so rollback is deterministic.",
                        );
                    }
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
    /// Manual mode does NOT install `CURRENT_TX`. Scope individual
    /// operations through the transaction with `Builder::with_tx(&tx)`
    /// or the `Model::*_with_tx(&tx, ...)` shims.
    ///
    /// Holding a `Transaction` pins one pool connection for its
    /// entire lifetime. Pre-load any rows you need to read BEFORE
    /// calling `begin_transaction`, especially on SQLite (where the
    /// single shared connection is checked out for the tx duration).
    pub async fn begin_transaction() -> Result<Transaction, FrameworkError> {
        let conn = DB::connection()?;
        let conn_name: Arc<str> = super::PRIMARY_CONNECTION_NAME.into();
        let tx = conn
            .inner()
            .begin()
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        emit_tx_event(super::events::TransactionBeginning {
            connection_name: conn_name.to_string(),
        })
        .await;
        Ok(Transaction {
            inner: Arc::new(tx),
            connection_name: conn_name,
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

/// Render every bound value of a `Statement` as a debug string. The
/// `format!("{val:?}")` representation is the same shape used by
/// `run_instrumented`; pulled out so `select_all`/`select_one`/
/// `select_count` can match.
fn stmt_bindings_strings(stmt: &sea_orm::Statement) -> Vec<String> {
    stmt.values
        .as_ref()
        .map(|v| v.0.iter().map(|val| format!("{val:?}")).collect())
        .unwrap_or_default()
}

/// Emit a `QueryExecuted` event from the captured SQL/bindings + an
/// execution result. Used by the `select_*` helpers; the result is
/// borrowed (not consumed) so the caller still returns it.
async fn finish_query_event<T>(
    sql: String,
    bindings: Vec<String>,
    elapsed: std::time::Duration,
    rw: super::events::ReadWriteType,
    connection_name: String,
    res: &Result<T, sea_orm::DbErr>,
) {
    let result_for_event: Result<(), String> = match res {
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    };
    let event = super::events::QueryExecuted {
        sql,
        bindings,
        time: elapsed,
        connection_name,
        read_write_type: Some(rw),
        result: result_for_event,
    };
    emit_query_executed(event).await;
}

/// Fan a [`QueryExecuted`](super::events::QueryExecuted) event out to
/// every registered observer. Three sinks:
///
/// 1. Direct `DB::listen(|q| { ... })` callbacks — synchronous.
/// 2. The in-memory query log when
///    [`DB::enable_query_log`](crate::DB::enable_query_log) is active.
/// 3. The framework-wide [`EventDispatcher`](crate::EventDispatcher)
///    so `EventFacade::listen::<QueryExecuted, _>(...)` works.
///
/// The whole call runs inside
/// [`with_dispatching_flag`](super::events::with_dispatching_flag) so
/// any listener that re-queries does not re-fire QueryExecuted.
/// Listener errors at the EventFacade layer are swallowed via
/// [`dispatch_best_effort`](crate::EventFacade::dispatch_best_effort) —
/// observation must never fail the query.
pub(crate) async fn emit_query_executed(event: super::events::QueryExecuted) {
    super::events::with_dispatching_flag(async move {
        // (1) Direct DB::listen callbacks. Cloning the registry's
        // listener Vec keeps the lock window tight; listeners are
        // Arc-cloned so calling them outside the lock is safe.
        //
        // Each callback runs inside a `catch_unwind` boundary — the
        // query already completed (we're emitting the post-execution
        // event), so a panicking listener must NOT unwind through the
        // executor and surface as a "query failed" error to the
        // caller. Mirrors the EventFacade `dispatch_best_effort`
        // contract: observation never fails the query.
        let callbacks: Vec<super::events::QueryListener> = match super::events::listeners().read() {
            Ok(reg) => reg.listeners.clone(),
            Err(_) => Vec::new(),
        };
        for cb in callbacks {
            let event_ref = &event;
            let cb_ref = &cb;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                cb_ref(event_ref);
            }));
            if let Err(payload) = result {
                let msg = payload
                    .downcast_ref::<&'static str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "<non-string panic payload>".to_string());
                tracing::warn!(
                    target: "suprnova::database",
                    panic = %msg,
                    sql = %event.sql,
                    "DB::listen callback panicked; ignoring (query already completed)",
                );
            }
        }
        // (2) Query log.
        if let Ok(mut log) = super::events::query_log().lock()
            && log.enabled
        {
            log.entries.push(event.clone());
        }
        // (3) EventFacade dispatch. Best-effort — a logging listener
        // returning Err must not fail the query.
        if crate::EventFacade::has_listeners::<super::events::QueryExecuted>() {
            let _ = crate::EventFacade::dispatch_best_effort(event).await;
        }
    })
    .await;
}

/// Fire a transaction-lifecycle event through `EventFacade`. Best-effort
/// dispatch — a failing listener does not abort the transaction. The
/// no-listeners short-circuit makes this a no-op in the common case.
async fn emit_tx_event<E: crate::Event>(event: E) {
    if crate::EventFacade::has_listeners::<E>() {
        let _ = crate::EventFacade::dispatch_best_effort(event).await;
    }
}

/// Whether `e`'s Display matches the deadlock / serialization-failure
/// pattern. Used by [`DB::transaction_with_attempts`] to decide
/// whether to retry.
fn is_deadlock(e: &FrameworkError) -> bool {
    let msg = format!("{e}");
    msg.contains("40001") || msg.contains("40P01") || msg.to_lowercase().contains("deadlock")
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
        assert!(!is_deadlock(&FrameworkError::database(
            "connection refused"
        )));
        assert!(!is_deadlock(&FrameworkError::internal("oops")));
    }
}
