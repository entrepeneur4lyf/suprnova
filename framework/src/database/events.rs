//! Database lifecycle events.
//!
//! Mirrors Laravel's `Illuminate\Database\Events\*` family: every
//! observable point in the database lifecycle (pool open, query
//! executed, transaction lifecycle) is a strongly-typed event payload
//! that flows through the global [`EventDispatcher`](crate::EventDispatcher).
//!
//! ## Wiring
//!
//! These events are fired automatically by the framework:
//!
//! - [`ConnectionEstablished`] — once per
//!   [`DbConnection::connect`](crate::DbConnection::connect).
//! - [`QueryExecuted`] — once per query that runs through the
//!   instrumented [`ExecutorChoice`](crate::database::transaction::ExecutorChoice)
//!   helpers. Covers the [`DB`](crate::DB) raw escapes
//!   (`select`/`select_one`/`scalar`/`insert`/`update`/`delete`/
//!   `statement`/`affecting_statement`/`unprepared`) and the model-less
//!   [`DbTableBuilder`](crate::DbTableBuilder). The Eloquent execution
//!   path matches `ExecutorChoice` arms directly today; adopting the
//!   helpers — and therefore the QueryExecuted hook — is tracked in the
//!   Eloquent module.
//! - [`TransactionBeginning`] / [`TransactionCommitted`] /
//!   [`TransactionRolledBack`] — fired by the closure form
//!   ([`DB::transaction`](crate::DB::transaction)),
//!   [`DB::transaction_with_attempts`](crate::DB::transaction_with_attempts),
//!   and the manual handles ([`DB::begin_transaction`](crate::DB::begin_transaction)
//!   plus [`Transaction::commit`](crate::Transaction::commit) /
//!   [`Transaction::rollback`](crate::Transaction::rollback)).
//!
//! ## Re-entrancy contract
//!
//! [`QueryExecuted`] is emitted under a task-local re-entrancy guard:
//! a listener that itself issues a database query won't re-fire
//! `QueryExecuted` from that nested query. This prevents the
//! "log-to-DB listener → emits event → log-to-DB → ..." loop.
//!
//! Listeners run through [`EventFacade::dispatch_best_effort`](crate::EventFacade::dispatch_best_effort):
//! a failing listener does NOT fail the query — the query already
//! succeeded. The listener's error is logged but never propagated.

use crate::Event;
use std::sync::Arc;
use std::time::Duration;

/// Connection opened — fired once per
/// [`DbConnection::connect`](crate::DbConnection::connect).
///
/// Carries the connection's logical name. The default pool is
/// `__primary__`; named pools registered through
/// [`DB::register_named`](crate::DB::register_named) carry the name the
/// caller passed.
#[derive(Debug, Clone)]
pub struct ConnectionEstablished {
    /// Logical connection name (e.g. `__primary__`, `__read_replica__`).
    pub connection_name: String,
}

impl Event for ConnectionEstablished {
    fn event_name() -> &'static str {
        "Database\\ConnectionEstablished"
    }
}

/// A single query was executed against the database.
///
/// Fires AFTER the query completes (successfully OR with an error —
/// see [`QueryExecuted::result`]). The wall-clock duration in
/// [`QueryExecuted::time`] measures the dispatch-to-completion window
/// inside the [`ExecutorChoice`](crate::database::transaction::ExecutorChoice)
/// helper, not network round-trip time.
#[derive(Debug, Clone)]
pub struct QueryExecuted {
    /// The SQL string (with backend-specific placeholders intact).
    pub sql: String,
    /// The bound parameter values, in dispatch order. Rendered through
    /// `format!("{value:?}")` for observability; reflectively typed
    /// access goes through the original `sea_orm::Value` only inside
    /// the executor helper.
    pub bindings: Vec<String>,
    /// Wall-clock duration the query spent inside the executor helper.
    pub time: Duration,
    /// Logical connection name the query ran against.
    pub connection_name: String,
    /// Whether this query ran through a read-routed code path. `Some(true)`
    /// for read-shape ops, `Some(false)` for write-shape ops, `None` for
    /// raw helpers that don't classify (matches Laravel's nullable enum).
    pub read_write_type: Option<ReadWriteType>,
    /// Outcome of the underlying SeaORM call. `Ok(())` on success,
    /// `Err(message)` when the call failed. Listeners observe the
    /// failure but the query error still propagates to the caller —
    /// see the [module docs](self) for the listener contract.
    pub result: Result<(), String>,
}

/// Whether a query was classified as a read or a write by the executor
/// dispatch. Mirrors Laravel's nullable `read|write` field on
/// `QueryExecuted::$readWriteType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadWriteType {
    /// Read-shape op — SELECT, COUNT, etc. Routed through
    /// [`ExecutorChoice::resolve_read`](crate::database::transaction::ExecutorChoice::resolve_read).
    Read,
    /// Write-shape op — INSERT, UPDATE, DELETE, DDL. Routed through
    /// [`ExecutorChoice::resolve_write`](crate::database::transaction::ExecutorChoice::resolve_write).
    Write,
}

impl QueryExecuted {
    /// Render the SQL with bindings inlined. Lossy convenience for
    /// log messages — escaping is debug-format (`{:?}`), NOT SQL-safe.
    /// Mirrors Laravel's `QueryExecuted::toRawSql()` shape.
    ///
    /// Placeholders are substituted left-to-right by lexical
    /// `?`-occurrence (MySQL / SQLite) or `$N` numeric indices
    /// (Postgres). When the binding count mismatches, the original SQL
    /// is returned unchanged.
    pub fn to_raw_sql(&self) -> String {
        if self.sql.contains('$') {
            // Postgres: $1, $2, ...
            let mut rendered = self.sql.clone();
            for (i, b) in self.bindings.iter().enumerate() {
                let needle = format!("${}", i + 1);
                rendered = rendered.replace(&needle, b);
            }
            rendered
        } else {
            // ? placeholders
            let mut out = String::with_capacity(self.sql.len());
            let mut iter = self.bindings.iter();
            for c in self.sql.chars() {
                if c == '?' {
                    if let Some(b) = iter.next() {
                        out.push_str(b);
                    } else {
                        out.push('?');
                    }
                } else {
                    out.push(c);
                }
            }
            out
        }
    }
}

impl Event for QueryExecuted {
    fn event_name() -> &'static str {
        "Database\\QueryExecuted"
    }
}

/// A transaction is about to begin (after `BEGIN` has been issued and
/// accepted). Fired by the closure form ([`DB::transaction`](crate::DB::transaction))
/// and the manual handle ([`DB::begin_transaction`](crate::DB::begin_transaction)).
#[derive(Debug, Clone)]
pub struct TransactionBeginning {
    /// Logical connection name the transaction is bound to.
    pub connection_name: String,
}

impl Event for TransactionBeginning {
    fn event_name() -> &'static str {
        "Database\\TransactionBeginning"
    }
}

/// A transaction was committed.
#[derive(Debug, Clone)]
pub struct TransactionCommitted {
    /// Logical connection name the transaction was bound to.
    pub connection_name: String,
}

impl Event for TransactionCommitted {
    fn event_name() -> &'static str {
        "Database\\TransactionCommitted"
    }
}

/// A transaction was rolled back. Fires for both explicit rollbacks
/// ([`Transaction::rollback`](crate::Transaction::rollback)) and
/// closure-error rollbacks inside [`DB::transaction`](crate::DB::transaction).
/// Does NOT fire for an implicit Drop-rollback of a leaked manual
/// transaction handle — SeaORM's `Drop` impl is synchronous and
/// can't reach the async event dispatcher.
#[derive(Debug, Clone)]
pub struct TransactionRolledBack {
    /// Logical connection name the transaction was bound to.
    pub connection_name: String,
}

impl Event for TransactionRolledBack {
    fn event_name() -> &'static str {
        "Database\\TransactionRolledBack"
    }
}

/// Open-connection count crossed a configured threshold. Fired by
/// monitoring tooling (the future `db:monitor` CLI command); not
/// emitted by the framework itself.
#[derive(Debug, Clone)]
pub struct DatabaseBusy {
    /// Logical connection name being monitored.
    pub connection_name: String,
    /// Observed open connection count at the time of the threshold
    /// breach.
    pub connections: u32,
}

impl Event for DatabaseBusy {
    fn event_name() -> &'static str {
        "Database\\DatabaseBusy"
    }
}

// ---- Internal re-entrancy guard for QueryExecuted dispatch ---------------

tokio::task_local! {
    /// Set while a listener for [`QueryExecuted`] is being dispatched.
    /// The executor helpers check this and skip emission when nested,
    /// preventing log-to-DB listeners from looping.
    pub(crate) static DISPATCHING_QUERY_EXECUTED: bool;
}

/// Run `f` with the [`DISPATCHING_QUERY_EXECUTED`] flag set. Used by
/// the listener dispatch path in
/// [`ExecutorChoice`](crate::database::transaction::ExecutorChoice).
pub(crate) async fn with_dispatching_flag<F, T>(f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    DISPATCHING_QUERY_EXECUTED.scope(true, f).await
}

/// Whether we're inside a listener dispatch right now. The executor
/// helpers consult this; `true` skips QueryExecuted emission to avoid
/// re-entry. Returns `false` outside a Tokio runtime context.
pub(crate) fn is_dispatching() -> bool {
    DISPATCHING_QUERY_EXECUTED.try_with(|v| *v).unwrap_or(false)
}

// ---- Cumulative query log ------------------------------------------------

/// Per-connection in-memory query log. Captures every dispatched
/// [`QueryExecuted`] when [`DB::enable_query_log`](crate::DB::enable_query_log)
/// is active. Drained via [`DB::get_query_log`](crate::DB::get_query_log).
#[derive(Debug, Default)]
pub(crate) struct QueryLog {
    pub(crate) enabled: bool,
    pub(crate) entries: Vec<QueryExecuted>,
}

static QUERY_LOG: std::sync::OnceLock<std::sync::Mutex<QueryLog>> = std::sync::OnceLock::new();

pub(crate) fn query_log() -> &'static std::sync::Mutex<QueryLog> {
    QUERY_LOG.get_or_init(|| std::sync::Mutex::new(QueryLog::default()))
}

// ---- DB::listen direct callback registry --------------------------------

/// Caller-friendly `DB::listen(|q| { ... })` closure type. Mirrors
/// Laravel's `DB::listen(function (QueryExecuted $event) { ... })`
/// signature.
pub type QueryListener = Arc<dyn Fn(&QueryExecuted) + Send + Sync + 'static>;

#[derive(Default)]
pub(crate) struct ListenerRegistry {
    pub(crate) listeners: Vec<QueryListener>,
}

static LISTENERS: std::sync::OnceLock<std::sync::RwLock<ListenerRegistry>> =
    std::sync::OnceLock::new();

pub(crate) fn listeners() -> &'static std::sync::RwLock<ListenerRegistry> {
    LISTENERS.get_or_init(|| std::sync::RwLock::new(ListenerRegistry::default()))
}

/// True when at least one source of [`QueryExecuted`] observation is
/// active: a direct `DB::listen` callback, an `EventFacade::listen`
/// listener, OR the query log is enabled. The executor helpers consult
/// this on every call — when nobody is listening the entire emission
/// path short-circuits and pays zero overhead.
pub(crate) fn query_observation_active() -> bool {
    if let Ok(reg) = listeners().read()
        && !reg.listeners.is_empty()
    {
        return true;
    }
    if let Ok(log) = query_log().lock()
        && log.enabled
    {
        return true;
    }
    crate::EventFacade::has_listeners::<QueryExecuted>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Event;

    #[test]
    fn event_names_are_stable() {
        assert_eq!(
            ConnectionEstablished::event_name(),
            "Database\\ConnectionEstablished"
        );
        assert_eq!(QueryExecuted::event_name(), "Database\\QueryExecuted");
        assert_eq!(
            TransactionBeginning::event_name(),
            "Database\\TransactionBeginning"
        );
        assert_eq!(
            TransactionCommitted::event_name(),
            "Database\\TransactionCommitted"
        );
        assert_eq!(
            TransactionRolledBack::event_name(),
            "Database\\TransactionRolledBack"
        );
        assert_eq!(DatabaseBusy::event_name(), "Database\\DatabaseBusy");
    }

    #[test]
    fn to_raw_sql_substitutes_question_marks_left_to_right() {
        let q = QueryExecuted {
            sql: "SELECT * FROM users WHERE id = ? AND active = ?".into(),
            bindings: vec!["42".into(), "true".into()],
            time: Duration::from_millis(1),
            connection_name: "__primary__".into(),
            read_write_type: Some(ReadWriteType::Read),
            result: Ok(()),
        };
        assert_eq!(
            q.to_raw_sql(),
            "SELECT * FROM users WHERE id = 42 AND active = true"
        );
    }

    #[test]
    fn to_raw_sql_substitutes_postgres_numeric_placeholders() {
        let q = QueryExecuted {
            sql: "SELECT * FROM users WHERE id = $1 AND active = $2".into(),
            bindings: vec!["42".into(), "true".into()],
            time: Duration::from_millis(1),
            connection_name: "__primary__".into(),
            read_write_type: Some(ReadWriteType::Read),
            result: Ok(()),
        };
        assert_eq!(
            q.to_raw_sql(),
            "SELECT * FROM users WHERE id = 42 AND active = true"
        );
    }

    #[test]
    fn to_raw_sql_with_no_bindings_returns_sql_unchanged() {
        let q = QueryExecuted {
            sql: "SELECT NOW()".into(),
            bindings: vec![],
            time: Duration::from_millis(1),
            connection_name: "__primary__".into(),
            read_write_type: Some(ReadWriteType::Read),
            result: Ok(()),
        };
        assert_eq!(q.to_raw_sql(), "SELECT NOW()");
    }
}
