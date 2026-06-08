//! `Builder<M>` — the chainable query type returned by `Model::query()`.
//!
//! ## Dual API
//!
//! Every where-shape method ships under TWO names:
//! - `filter*` (Rust-idiomatic) — primary, the implementation lives here.
//! - `db_where` / `where_*` (Laravel-faithful) — one-line aliases that
//!   delegate. Tagged with `#[doc(alias = "...")]` so rustdoc search
//!   finds either.
//!
//! Pick whichever your muscle memory matches. Both compile, both produce
//! identical SQL.
//!
//! ## SQL identifier contract (security)
//!
//! Column names taken through [`IntoColumn`] — the trait that backs
//! every `filter*` / `where_*` / `order_by` / `group_by` / `having*`
//! method — are interpolated **raw** into the rendered SQL. There is
//! no quoting or escaping at this layer; matches Laravel's
//! `DB::table()->where(...)` contract exactly.
//!
//! Therefore: **never accept a column name from untrusted input**
//! (URL params, request body, query strings). Hardcode the column or
//! select it from a known allowlist before passing it to the builder.
//! The right-hand-side of comparisons goes through [`IntoVal`] and
//! becomes a parameterised SQL bind — those values ARE safe to take
//! from untrusted input.
//!
//! The raw-SQL escape hatches [`Builder::where_raw`],
//! [`Builder::order_by_raw`], [`Builder::select_raw`] (and their
//! `filter_raw` aliases) extend the same contract: the raw SQL
//! fragment is interpolated verbatim; only the positional bindings
//! Vec is parameterised.
//!
//! ## Per-WhereTerm SQL renderer
//!
//! `Builder::render_select_for` emits per-backend SQL from the
//! `WhereTerm` AST: Postgres `$N` placeholders + `EXTRACT(... FROM
//! col)` date parts + `@>` JSON containment; MySQL + SQLite use `?`
//! placeholders with backend-appropriate `DATE()` / `JSON_LENGTH()`
//! forms.
//!
//! UNION arms thread the placeholder counter through
//! `Builder::render_select_into` so Postgres `$N` numbering stays
//! monotonic across the combined statement — see
//! `union_postgres_placeholders_are_monotonic` in
//! `framework/tests/eloquent_builder.rs` for the regression test.

use std::any::Any;
use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::RangeInclusive;

use chrono::{NaiveDate, NaiveTime};
use sea_orm::{
    ConnectionTrait, DbBackend, FromQueryResult, Statement, TryGetable, Value as SeaValue,
};
use serde_json::Value;

use crate::database::DB;
use crate::eloquent::EloquentModel;
use crate::eloquent::attrs::Attrs;
use crate::eloquent::collection::Collection;
use crate::eloquent::model::{Model, json_value_to_sea_value};
use crate::error::FrameworkError;

// ---- IntoColumn / IntoVal ------------------------------------------------

/// Convert a value into a column name for use with `Builder<M>` methods.
/// Implemented by every macro-generated `Column` enum so users can write
/// either typed (`Column::Email`) or string (`"email"`) arguments
/// throughout the builder API.
///
/// # Security: column names are SQL identifiers, not parameters
///
/// The string returned by `col_name()` is interpolated **raw** into the
/// rendered SQL — there is no quoting or escaping at this layer (same
/// contract as Laravel's `DB::table()->where(...)`). Anywhere
/// `IntoColumn` appears in the public surface, **never accept the value
/// from untrusted input** (URL params, request body, query strings).
/// Hardcode the column or pick it from a known allowlist before
/// calling the builder.
///
/// Values (the right-hand side of comparisons, IN lists, BETWEEN
/// bounds, etc.) go through [`IntoVal`] → `serde_json::Value` and
/// become parameterised binds — those ARE safe to take from untrusted
/// input.
pub trait IntoColumn {
    /// Return the snake-case column name as a `String`. Owned because
    /// the typed-enum impl materialises a new string from a
    /// `&'static str` accessor.
    fn col_name(self) -> String;
}

impl IntoColumn for &str {
    fn col_name(self) -> String {
        self.to_string()
    }
}

impl IntoColumn for String {
    fn col_name(self) -> String {
        self
    }
}

impl IntoColumn for &String {
    fn col_name(self) -> String {
        self.clone()
    }
}

/// Convert a value into a `serde_json::Value` for use as a SQL bind.
/// Anything `Into<Value>` works (numbers, strings, bools, vectors,
/// `serde_json::Value` itself), which covers every primitive the
/// builder accepts.
pub trait IntoVal {
    /// Convert `self` into a [`serde_json::Value`] suitable for binding into a SQL query.
    fn into_val(self) -> Value;
}

impl<T: Into<Value>> IntoVal for T {
    fn into_val(self) -> Value {
        self.into()
    }
}

// ---- Direction + AST -----------------------------------------------------

/// SQL ordering direction. Used by [`Builder::order_by`] and the
/// `order_by_desc` / `order_by_asc` shortcuts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Ascending order (`ASC`).
    Asc,
    /// Descending order (`DESC`).
    Desc,
}

impl Direction {
    fn sql(&self) -> &'static str {
        match self {
            Direction::Asc => "ASC",
            Direction::Desc => "DESC",
        }
    }
}

/// One clause in the WHERE tree. The builder accumulates these and
/// the renderer walks them to emit per-backend SQL.
#[derive(Debug, Clone)]
pub(crate) enum WhereTerm {
    Eq(String, Value),
    Op(String, String, Value),
    In(String, Vec<Value>),
    NotIn(String, Vec<Value>),
    Between(String, Value, Value),
    NotBetween(String, Value, Value),
    Null(String),
    NotNull(String),
    Like(String, String),
    NotLike(String, String),
    Column(String, String),
    Raw(String, Vec<Value>),
    JsonContains(String, Value),
    JsonLength(String, String, i64),
    DatePart(DatePart, String, Value),
    Not(Box<WhereTerm>),
    Or(Vec<WhereTerm>),
    /// Correlated `EXISTS (...)` / `NOT EXISTS (...)` from
    /// [`Builder::has`] / [`Builder::where_has`] /
    /// [`Builder::doesnt_have`] / [`Builder::where_doesnt_have`] and the
    /// belongs-to / morph existence variants. The boxed spec carries
    /// everything the renderer needs: the relation entry's join shape,
    /// any user-supplied inner WHERE terms, the EXISTS/NOT-EXISTS
    /// polarity, and the optional `>= N` count constraint.
    Exists(Box<ExistsSpec>),
}

/// Spec passed to [`WhereTerm::Exists`]. Built by
/// [`Builder::has`] / [`Builder::where_has`] / [`Builder::doesnt_have`]
/// / [`Builder::where_doesnt_have`] and the belongs-to / morph
/// existence variants. The boxed shape keeps `WhereTerm` small.
#[derive(Debug, Clone)]
pub(crate) struct ExistsSpec {
    /// The parent's `EloquentModel::TABLE` — passed in so the renderer
    /// can qualify the correlated `parent.pk = child.fk` clause.
    pub parent_table: String,
    /// The parent's PK column on this side of the EXISTS — taken from
    /// [`RelationEntry::parent_key`].
    pub parent_key: String,
    /// Direct target table for has / has-one / has-many / morph-one /
    /// morph-many. Empty when the join routes through a pivot — see
    /// `pivot_table`.
    pub target_table: String,
    /// FK on the child / target side. Empty for pivot joins.
    pub foreign_key: String,
    /// Pivot table for m2m / morph-m2m / through. Empty otherwise.
    pub pivot_table: String,
    /// Pivot column pointing at the parent. Empty when no pivot.
    pub pivot_parent_key: String,
    /// Pivot column pointing at the related / final target. Empty
    /// when no pivot.
    pub pivot_related_key: String,
    /// Related table's PK (for joining pivot → related). Empty when no
    /// pivot path.
    pub related_pk: String,
    /// Morph discriminator column on the child side. Empty for
    /// non-morph kinds.
    pub morph_type_column: String,
    /// Stable morph-type string the discriminator must equal. Empty
    /// for non-morph kinds.
    pub morph_type_value: String,
    /// Soft-delete column on the related model (`deleted_at` by default,
    /// or the model's custom `soft_deletes_column`). Empty when the
    /// related model does not soft-delete. When present, the renderer
    /// appends `<target>.<col> IS NULL` to the EXISTS subquery WHERE so
    /// `has`/`where_has` agrees with the related model's default scope.
    pub related_soft_deletes_column: String,
    /// Polarity: `true` renders `EXISTS (...)`, `false` renders
    /// `NOT EXISTS (...)`.
    pub positive: bool,
    /// Optional `>= count` constraint (Laravel-shape: `has("posts",
    /// ">", 3)`). When `None` the subquery is bare `EXISTS (SELECT 1
    /// ...)`. When `Some`, the renderer expands to `(SELECT COUNT(*) ...)
    /// op count`, with `op` being a validated SQL comparison operator.
    pub count_op: Option<String>,
    pub count_value: Option<i64>,
    /// Optional inner WHERE constraint contributed by `where_has`'s
    /// closure. The inner builder's `where_terms` are merged into the
    /// subquery body so users can refine the EXISTS arm.
    pub inner_terms: Vec<WhereTerm>,
    /// Phase 10D — Laravel `whereRelation("posts", "col", "val")`
    /// shortcut. Renders the same EXISTS shape but adds the col/op/val
    /// constraint inline; equivalent to `whereHas` with a tiny closure.
    pub relation_column: Option<String>,
    pub relation_op: Option<String>,
    pub relation_value: Option<Value>,
}

/// Which part of a temporal column to compare against. Mapped per
/// backend by [`render_date_part`].
#[derive(Debug, Clone, Copy)]
pub(crate) enum DatePart {
    Date,
    Day,
    Month,
    Year,
    Time,
}

/// One entry in the ORDER BY list.
#[derive(Debug, Clone)]
pub(crate) enum OrderTerm {
    Col(String, Direction),
    Raw(String),
    Random,
}

/// Row-locking hint applied to a SELECT.
///
/// Set via [`Builder::lock_for_update`] / [`Builder::shared_lock`] and
/// consumed by [`Builder::render_select_for`], which appends the
/// backend-appropriate clause to the end of the compound statement.
///
/// SQLite has no row-level locking — the methods compile but emit no
/// SQL on that backend (and log a `warn!` once per process so a
/// misconfigured app surfaces the no-op).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockMode {
    /// No row lock — the default.
    None,
    /// Exclusive write lock: `SELECT ... FOR UPDATE` on Postgres + MySQL.
    /// Other transactions trying to lock the same rows block until the
    /// holding transaction commits or rolls back.
    ForUpdate,
    /// Shared read lock: `SELECT ... FOR SHARE` (Postgres) or
    /// `SELECT ... LOCK IN SHARE MODE` (MySQL). Allows concurrent
    /// shared readers; blocks concurrent `FOR UPDATE` writers.
    Shared,
}

// ---- Eager-load spec -----------------------------------------------------

/// One entry in a [`Builder<M>`]'s eager-load plan. Built by the
/// `with` / `with_count` / `with_sum/avg/min/max` / `with_where`
/// methods and consumed at [`Builder::get`] time by the eager-load
/// orchestrator in [`crate::eloquent::relations::eager`].
///
/// `WithWhere`'s closure is type-erased through `Box<dyn Any + Send +
/// Sync>` because the relation's target type is only known at
/// dispatch time. The per-relation `__eager_load` match arm downcasts
/// to the concrete `Box<dyn FnOnce(Builder<R>) -> Builder<R>>` before
/// applying.
pub(crate) enum EagerSpec {
    /// `with(["posts"])` or `with(["posts.comments"])`. The dotted form
    /// drives nested-path recursion through `__recurse_eager_load`.
    With(String),
    /// `with_count(["posts"])` — emits a server-side `COUNT(*)
    /// GROUP BY` for the relation.
    WithCount(String),
    /// `with_sum(("posts", "views"))` — server-side `SUM(col)
    /// GROUP BY`.
    WithSum(String, String),
    /// `with_avg(("posts", "views"))` — server-side `AVG(col)
    /// GROUP BY`.
    WithAvg(String, String),
    /// `with_min(("posts", "views"))` — server-side `MIN(col)
    /// GROUP BY`.
    WithMin(String, String),
    /// `with_max(("posts", "views"))` — server-side `MAX(col)
    /// GROUP BY`.
    WithMax(String, String),
    /// `with_where(("posts", |q: Builder<Post>| q.filter(...)))`.
    ///
    /// The closure is type-erased to `Box<dyn Any + Send + Sync>`
    /// here. The per-relation `__eager_load` arm knows the concrete
    /// target type and downcasts before applying. The user supplies a
    /// monomorphic closure at the call site (the parameter type
    /// `Builder<R>` is the relation target), so the downcast cannot
    /// fail on a well-typed program.
    WithWhere(String, Box<dyn Any + Send + Sync>),
}

impl std::fmt::Debug for EagerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EagerSpec::With(name) => f.debug_tuple("With").field(name).finish(),
            EagerSpec::WithCount(name) => f.debug_tuple("WithCount").field(name).finish(),
            EagerSpec::WithSum(name, col) => {
                f.debug_tuple("WithSum").field(name).field(col).finish()
            }
            EagerSpec::WithAvg(name, col) => {
                f.debug_tuple("WithAvg").field(name).field(col).finish()
            }
            EagerSpec::WithMin(name, col) => {
                f.debug_tuple("WithMin").field(name).field(col).finish()
            }
            EagerSpec::WithMax(name, col) => {
                f.debug_tuple("WithMax").field(name).field(col).finish()
            }
            EagerSpec::WithWhere(name, _) => f
                .debug_struct("WithWhere")
                .field("relation", name)
                .field("predicate", &"<closure>")
                .finish(),
        }
    }
}

// ---- Builder -------------------------------------------------------------

/// The chainable query type. Constructed via `Model::query()` or one of
/// the static shortcuts the `#[suprnova::model]` macro emits on the
/// user struct (`T5User::filter(...)`, `T5User::where_in(...)`, ...).
pub struct Builder<M> {
    pub(crate) where_terms: Vec<WhereTerm>,
    pub(crate) orders: Vec<OrderTerm>,
    pub(crate) select_cols: Option<Vec<String>>,
    pub(crate) select_raw: Option<String>,
    pub(crate) group_by: Vec<String>,
    pub(crate) having_terms: Vec<WhereTerm>,
    pub(crate) limit: Option<u64>,
    pub(crate) offset: Option<u64>,
    pub(crate) distinct: bool,
    pub(crate) unions: Vec<(Box<Builder<M>>, bool)>, // (other, is_union_all)
    pub(crate) runtime_casts:
        HashMap<&'static str, std::sync::Arc<dyn crate::eloquent::casts::DynCast>>,
    pub(crate) global_scopes_disabled: Vec<&'static str>,
    /// Phase 10C T4 — typed global-scope opt-out mask. Each entry is
    /// the `TypeId` of a scope struct registered via
    /// [`ScopeRegistry::register`]; the registry skips any scope whose
    /// id appears here.
    ///
    /// [`ScopeRegistry::register`]: crate::eloquent::ScopeRegistry::register
    pub(crate) excluded_scopes: Vec<std::any::TypeId>,
    /// Phase 10C T4 — if true, the registered global scopes for this
    /// model are bypassed entirely on this query.
    pub(crate) skip_all_scopes: bool,
    /// Eager-load plan — populated by [`Builder::with`] /
    /// [`Builder::with_count`] / [`Builder::with_sum`] /
    /// [`Builder::with_avg`] / [`Builder::with_min`] /
    /// [`Builder::with_max`] / [`Builder::with_where`].
    ///
    /// At [`Builder::get`] time the orchestrator in
    /// [`crate::eloquent::relations::eager`] walks each entry and
    /// dispatches into the per-model `__eager_load` /
    /// `__count_relation` / `__aggregate_relation` methods. Dotted
    /// `"posts.comments"` paths recurse through
    /// `__recurse_eager_load`.
    pub(crate) eager_specs: Vec<EagerSpec>,
    /// Phase 10C T9 — row-locking hint applied at SQL emission time.
    /// Set via [`Builder::lock_for_update`] / [`Builder::shared_lock`].
    /// The clause is appended at the very end of the compound
    /// statement by [`Builder::render_select_for`] — after every
    /// UNION arm — so multi-statement queries lock once at the outer
    /// scope, matching standard SQL grammar.
    pub(crate) lock_mode: LockMode,
    /// Phase 10C T11 — per-builder transaction override. When set,
    /// every terminal method routes through this transaction instead
    /// of consulting the [`CURRENT_TX`](crate::database::transaction::CURRENT_TX)
    /// task-local or falling back to [`DB::connection()`]. Installed
    /// by [`Builder::with_tx`].
    pub(crate) tx_override: Option<crate::database::TxHandle>,
    /// Phase 10C T12 — per-builder connection override. When set,
    /// terminal methods route through the named connection in the
    /// [`ConnectionRegistry`](crate::database::ConnectionRegistry).
    /// Installed by [`Builder::on`] / [`Builder::on_write_connection`].
    /// Bypassed by an active transaction (closure form CURRENT_TX or
    /// explicit `with_tx`) — transactions take precedence absolutely.
    pub(crate) connection_override: Option<String>,
    _phantom: PhantomData<M>,
}

impl<M> Default for Builder<M> {
    fn default() -> Self {
        Self::new()
    }
}

/// Manual `Clone` for `Builder<M>` — every field is `Clone` except
/// `EagerSpec::WithWhere`, whose `Box<dyn Any>` payload (the
/// type-erased relation predicate) is not. Phase 10C T8's chunking
/// and lazy iteration both need the builder to clone across query
/// boundaries; rather than tighten `with_where`'s `FnOnce` bound to
/// `Fn` (which would touch every macro-emitted relation arm), the
/// clone drops `eager_specs` entirely.
///
/// The drop is safe because every chunking entry point
/// ([`Builder::chunk`], [`Builder::chunk_by_id`], [`Builder::lazy`])
/// asserts `eager_specs.is_empty()` up front and returns
/// `FrameworkError::internal` otherwise — so user code that pairs
/// `.with(...)` with `.chunk(...)` gets a loud error instead of a
/// silent eager-load drop. Users who need eager loading inside a
/// chunked walk re-apply `.with(...)` inside the per-chunk closure.
impl<M> Clone for Builder<M> {
    fn clone(&self) -> Self {
        Self {
            where_terms: self.where_terms.clone(),
            orders: self.orders.clone(),
            select_cols: self.select_cols.clone(),
            select_raw: self.select_raw.clone(),
            group_by: self.group_by.clone(),
            having_terms: self.having_terms.clone(),
            limit: self.limit,
            offset: self.offset,
            distinct: self.distinct,
            unions: self.unions.clone(),
            runtime_casts: self.runtime_casts.clone(),
            global_scopes_disabled: self.global_scopes_disabled.clone(),
            excluded_scopes: self.excluded_scopes.clone(),
            skip_all_scopes: self.skip_all_scopes,
            // EagerSpec::WithWhere holds a non-Clone Box<dyn Any>; drop
            // the plan on clone. Chunking entry points error-check
            // before they clone, so users see the violation instead of
            // a silent drop.
            eager_specs: Vec::new(),
            lock_mode: self.lock_mode,
            // T11: transaction override is a cheap `Arc` clone — every
            // clone of the builder targets the same underlying tx.
            tx_override: self.tx_override.clone(),
            // T12: per-builder connection override carries through
            // clones (chunk / lazy / clone-to-modify patterns) so the
            // routing stays consistent across the cloned query family.
            connection_override: self.connection_override.clone(),
            _phantom: PhantomData,
        }
    }
}

/// Walk a [`WhereTerm`] and validate every identifier + operator it
/// carries. Free function (not a method) so [`Builder::validate_inputs`]
/// can recurse via `Not` / `Or` without monomorphisation noise on
/// `M`. See [`Builder::validate_inputs`] for the full contract.
fn validate_where_term(term: &WhereTerm) -> Result<(), FrameworkError> {
    use crate::database::{validate_identifier, validate_sql_operator};
    match term {
        WhereTerm::Eq(c, _)
        | WhereTerm::In(c, _)
        | WhereTerm::NotIn(c, _)
        | WhereTerm::Between(c, _, _)
        | WhereTerm::NotBetween(c, _, _)
        | WhereTerm::Null(c)
        | WhereTerm::NotNull(c)
        | WhereTerm::Like(c, _)
        | WhereTerm::NotLike(c, _)
        | WhereTerm::JsonContains(c, _)
        | WhereTerm::DatePart(_, c, _) => {
            validate_identifier(c)?;
        }
        WhereTerm::Op(c, op, _) | WhereTerm::JsonLength(c, op, _) => {
            validate_identifier(c)?;
            validate_sql_operator(op)?;
        }
        WhereTerm::Column(a, b) => {
            validate_identifier(a)?;
            validate_identifier(b)?;
        }
        WhereTerm::Raw(_, _) => {
            // Explicit raw-SQL escape hatch; caller documents the
            // trust boundary at `Builder::where_raw` /
            // `Builder::having_raw`.
        }
        WhereTerm::Not(inner) => validate_where_term(inner)?,
        WhereTerm::Or(terms) => {
            for t in terms {
                validate_where_term(t)?;
            }
        }
        WhereTerm::Exists(spec) => {
            // The renderer only interpolates spec fields the macro
            // populated from compile-time string literals or fully
            // validated overrides. We still re-run the identifier /
            // operator validator at the I/O boundary to enforce
            // the contract documented on
            // [`Builder::has`] / [`Builder::where_relation`]: even
            // metadata fed from `RelationEntry` (which is otherwise
            // trusted) is treated as untrusted at this layer.
            if !spec.parent_table.is_empty() {
                validate_identifier(&spec.parent_table)?;
            }
            if !spec.parent_key.is_empty() {
                validate_identifier(&spec.parent_key)?;
            }
            if !spec.target_table.is_empty() {
                validate_identifier(&spec.target_table)?;
            }
            if !spec.foreign_key.is_empty() {
                validate_identifier(&spec.foreign_key)?;
            }
            if !spec.pivot_table.is_empty() {
                validate_identifier(&spec.pivot_table)?;
            }
            if !spec.pivot_parent_key.is_empty() {
                validate_identifier(&spec.pivot_parent_key)?;
            }
            if !spec.pivot_related_key.is_empty() {
                validate_identifier(&spec.pivot_related_key)?;
            }
            if !spec.related_pk.is_empty() {
                validate_identifier(&spec.related_pk)?;
            }
            if !spec.morph_type_column.is_empty() {
                validate_identifier(&spec.morph_type_column)?;
            }
            if let Some(op) = &spec.count_op {
                validate_sql_operator(op)?;
            }
            if let Some(col) = &spec.relation_column {
                validate_identifier(col)?;
            }
            if let Some(op) = &spec.relation_op {
                validate_sql_operator(op)?;
            }
            for t in &spec.inner_terms {
                validate_where_term(t)?;
            }
        }
    }
    Ok(())
}

impl<M> Builder<M> {
    /// Walk the builder's accumulated identifiers and operators and
    /// reject any that don't pass
    /// [`crate::database::validate_identifier`] /
    /// [`crate::database::validate_sql_operator`]. Called from every
    /// public terminal method (`get`, `first`, `count`, `exists`, the
    /// paginators, chunk / lazy, the aggregate helpers) before SQL is
    /// rendered.
    ///
    /// Audit HIGH `eloquent` #1: `IntoColumn` accepts `&str` /
    /// `String` as a passthrough — so even though the typed-column
    /// path exists, every fluent method also accepts opaque strings.
    /// Without validation, user-controlled strings reach the SQL
    /// renderer verbatim. The validator runs once per terminal at the
    /// I/O boundary; the fluent builder methods stay infallible.
    ///
    /// `select_raw`, `WhereTerm::Raw`, and `OrderTerm::Raw` are
    /// explicit raw-SQL escape hatches — their docs warn callers
    /// about the trust boundary, and validation deliberately skips
    /// them (otherwise the escape hatch wouldn't exist).
    pub(crate) fn validate_inputs(&self) -> Result<(), FrameworkError> {
        use crate::database::validate_identifier;

        if let Some(cols) = &self.select_cols {
            for c in cols {
                validate_identifier(c)?;
            }
        }
        for c in &self.group_by {
            validate_identifier(c)?;
        }
        for term in self.where_terms.iter().chain(self.having_terms.iter()) {
            validate_where_term(term)?;
        }
        for o in &self.orders {
            match o {
                OrderTerm::Col(c, _) => {
                    validate_identifier(c)?;
                }
                OrderTerm::Raw(_) | OrderTerm::Random => {
                    // Explicit escape hatch / framework literal.
                }
            }
        }
        // UNION arms must also pass.
        for (other, _is_all) in &self.unions {
            other.validate_inputs()?;
        }
        Ok(())
    }

    /// Construct an empty builder. Each `Model::query()` call returns
    /// a fresh instance.
    pub fn new() -> Self {
        Self {
            where_terms: Vec::new(),
            orders: Vec::new(),
            select_cols: None,
            select_raw: None,
            group_by: Vec::new(),
            having_terms: Vec::new(),
            limit: None,
            offset: None,
            distinct: false,
            unions: Vec::new(),
            runtime_casts: HashMap::new(),
            global_scopes_disabled: Vec::new(),
            excluded_scopes: Vec::new(),
            skip_all_scopes: false,
            eager_specs: Vec::new(),
            lock_mode: LockMode::None,
            tx_override: None,
            connection_override: None,
            _phantom: PhantomData,
        }
    }

    /// Phase 10C T12 — route every terminal method on this builder
    /// through the connection registered under `name` (via
    /// [`DB::register_named`](crate::DB::register_named) or
    /// [`ConnectionRegistry::register_existing`](crate::database::ConnectionRegistry::register_existing)).
    ///
    /// Precedence: active transaction > builder `with_tx` >
    /// `on(name)` > per-model `#[model(connection = "...")]` >
    /// `__read_replica__` auto-routing > default pool. Inside a
    /// [`DB::transaction`](crate::DB::transaction) closure the
    /// transaction's connection wins — `on(name)` is silently ignored,
    /// because every operation inside the closure must commit / roll
    /// back atomically through the same physical connection.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // Register an analytics replica at boot.
    /// DB::register_named("analytics_read", analytics_config).await?;
    ///
    /// // Per-query routing — read sales totals from the analytics replica.
    /// let totals = Order::query()
    ///     .filter_op("created_at", ">=", start)
    ///     .on("analytics_read")
    ///     .sum::<f64>("total")
    ///     .await?;
    /// ```
    pub fn on(mut self, name: impl Into<String>) -> Self {
        self.connection_override = Some(name.into());
        self
    }

    /// Phase 10C T12 — opt this builder back to the primary pool, even
    /// when a `__read_replica__` is registered and would normally take
    /// reads. Use this for read-your-writes scenarios where the replica
    /// might not have caught up yet.
    ///
    /// Equivalent to `.on("__primary__")`. The framework recognises the
    /// `__primary__` sentinel and short-circuits to
    /// [`DB::connection`](crate::DB::connection) without consulting the
    /// registry.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // Just inserted a user; read it back from primary so we see the row.
    /// User::create(suprnova::attrs! { email: "a@b.com" }).await?;
    /// let same = User::on_write_connection()
    ///     .filter("email", "a@b.com")
    ///     .first()
    ///     .await?;
    /// ```
    pub fn on_write_connection(mut self) -> Self {
        self.connection_override = Some(crate::database::PRIMARY_CONNECTION_NAME.to_string());
        self
    }

    /// Scope every terminal method on this builder through `tx`'s
    /// connection instead of consulting the ambient transaction or
    /// the global pool. Phase 10C T11.
    ///
    /// Precedence: explicit `with_tx` > ambient `CURRENT_TX`
    /// task-local > [`DB::connection()`]. Use `with_tx` when you have
    /// a manual [`Transaction`](crate::Transaction) (from
    /// [`DB::transaction`](crate::DB::transaction)) and want a
    /// specific query scoped to it without installing the task-local.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let tx = DB::begin_transaction().await?;
    /// let users = User::query().filter("active", true).with_tx(&tx).get().await?;
    /// tx.commit().await?;
    /// ```
    pub fn with_tx(mut self, tx: &crate::database::Transaction) -> Self {
        self.tx_override = Some(tx.handle());
        self
    }

    /// Append relation names to the eager-load plan.
    ///
    /// Flat names (`"posts"`) load the relation directly. Dotted
    /// names (`"posts.comments"`) drive nested-path recursion — the
    /// loader runs `__eager_load("posts", ...)` then walks each
    /// loaded post with `__recurse_eager_load("posts", "comments",
    /// ...)`. Paths nest as deep as the user wants:
    /// `"posts.comments.author"` runs three queries.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // Load every user's posts (one query) and every post's comments
    /// // (one query), then the author of each comment (one query).
    /// // Three queries total, zero N+1.
    /// let users = User::with(["posts.comments.author"]).get().await?;
    /// ```
    pub fn with<I, S>(mut self, relations: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for r in relations {
            self.eager_specs.push(EagerSpec::With(r.into()));
        }
        self
    }

    /// Append relation names whose `COUNT(*)` aggregate should be
    /// loaded alongside the parent rows. Reads from the cache via the
    /// macro-emitted `<rel>_count()` accessor.
    ///
    /// One server-side `GROUP BY` query per relation — independent of
    /// the parent row count.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let users = User::with_count(["posts"]).get().await?;
    /// for u in &users {
    ///     println!("{} has {} posts", u.name, u.posts_count());
    /// }
    /// ```
    pub fn with_count<I, S>(mut self, relations: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for r in relations {
            self.eager_specs.push(EagerSpec::WithCount(r.into()));
        }
        self
    }

    /// Append a `SUM(col) GROUP BY parent_fk` aggregate over a
    /// relation's column. Reads back via the wide `<rel>_<kind>_<col>`
    /// cache key — i.e.
    /// `parent.__eager.get_aggregate::<f64>("posts_sum_views")` for
    /// `with_sum(("posts", "views"))`. Sum/Avg over zero rows lands
    /// as `0.0`, matching the framework's COALESCE behaviour
    /// elsewhere.
    ///
    /// The ergonomic read is the macro-emitted accessor:
    /// `parent.posts_sum_of("views")` returning `Option<f64>` (`None`
    /// when `with_sum` was not called for that relation/column pair).
    /// Multiple aggregates on the same relation compose cleanly —
    /// `with_sum(("posts","views"))` and `with_avg(("posts","views"))`
    /// on the same query write distinct cells and both read back.
    pub fn with_sum<S1: Into<String>, S2: Into<String>>(mut self, (rel, col): (S1, S2)) -> Self {
        self.eager_specs
            .push(EagerSpec::WithSum(rel.into(), col.into()));
        self
    }

    /// Append an `AVG(col) GROUP BY parent_fk` aggregate over a
    /// relation's column. Storage: `f64`, defaults to `0.0` on empty
    /// groups.
    pub fn with_avg<S1: Into<String>, S2: Into<String>>(mut self, (rel, col): (S1, S2)) -> Self {
        self.eager_specs
            .push(EagerSpec::WithAvg(rel.into(), col.into()));
        self
    }

    /// Append a `MIN(col) GROUP BY parent_fk` aggregate over a
    /// relation's column. Storage: `Option<f64>` (matches SQL's
    /// `NULL`-on-empty + the [`Self::min`] terminal's shape).
    pub fn with_min<S1: Into<String>, S2: Into<String>>(mut self, (rel, col): (S1, S2)) -> Self {
        self.eager_specs
            .push(EagerSpec::WithMin(rel.into(), col.into()));
        self
    }

    /// Append a `MAX(col) GROUP BY parent_fk` aggregate over a
    /// relation's column. Storage: `Option<f64>`.
    pub fn with_max<S1: Into<String>, S2: Into<String>>(mut self, (rel, col): (S1, S2)) -> Self {
        self.eager_specs
            .push(EagerSpec::WithMax(rel.into(), col.into()));
        self
    }

    /// Constrain an eager-loaded relation with a builder predicate.
    /// The closure runs against the relation's inner `Builder<R>`
    /// before the IN-query lands, so only matching child rows are
    /// loaded into the eager cache.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let users = User::query()
    ///     .with_where(("posts", |q: Builder<Post>| q.filter("published", true)))
    ///     .get()
    ///     .await?;
    /// // Each u.posts_loaded() contains only published posts.
    /// ```
    ///
    /// The closure is type-erased to `Box<dyn Any>` for routing; the
    /// per-relation dispatcher arm downcasts back to
    /// `Box<dyn FnOnce(Builder<R>) -> Builder<R>>` at the match arm.
    /// User code writes a monomorphic closure (the parameter type is
    /// the relation's target), so the cast cannot fail on a
    /// well-typed program.
    pub fn with_where<S, R, F>(mut self, (rel, predicate): (S, F)) -> Self
    where
        S: Into<String>,
        // R only needs to be a static type for the type-erased Box,
        // not a full `Model`. The user-side closure is monomorphic in
        // the relation's target type — the bound only requires that
        // the predicate is well-typed against `Builder<R>` at the
        // call site. The dispatcher arm match for the relation knows
        // R statically and downcasts safely.
        R: 'static,
        F: FnOnce(Builder<R>) -> Builder<R> + Send + Sync + 'static,
    {
        // Erase the typed closure into the `Box<dyn Any>` slot. The
        // box stores a `Box<dyn FnOnce(Builder<R>) -> Builder<R>>` —
        // a fully-typed payload. The dispatcher arm match against the
        // relation name knows R statically and downcasts back to the
        // same shape.
        let boxed: Box<dyn FnOnce(Builder<R>) -> Builder<R> + Send + Sync + 'static> =
            Box::new(predicate);
        let erased: Box<dyn Any + Send + Sync> = Box::new(boxed);
        self.eager_specs
            .push(EagerSpec::WithWhere(rel.into(), erased));
        self
    }

    /// Folder helper: merge a fresh [`WhereTerm`] into an OR group
    /// with the previous WHERE term, matching the shape
    /// [`Self::or_filter`] produces.
    fn merge_or_term(&mut self, new: WhereTerm) {
        match self.where_terms.last_mut() {
            Some(WhereTerm::Or(group)) => group.push(new),
            Some(_) => {
                let last = self.where_terms.pop().expect("checked Some above");
                self.where_terms.push(WhereTerm::Or(vec![last, new]));
            }
            None => self.where_terms.push(new),
        }
    }

    /// Append the `(column, value)` pairs from an `Attrs` map onto the
    /// builder's WHERE clauses. Used by `first_or_create` /
    /// `update_or_create` / `first_or_new` to look a row up by exact
    /// attribute equality.
    pub(crate) fn filter_attrs(mut self, attrs: &Attrs) -> Self {
        for (k, v) in attrs.iter() {
            self.where_terms
                .push(WhereTerm::Eq(k.to_string(), v.clone()));
        }
        self
    }

    // ---- Equality / arbitrary operator -----------------------------------

    /// `WHERE col = val`.
    #[doc(alias = "db_where")]
    pub fn filter(mut self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.where_terms
            .push(WhereTerm::Eq(col.col_name(), val.into_val()));
        self
    }

    /// Laravel-shape alias for [`Self::filter`].
    #[doc(alias = "filter")]
    pub fn db_where(self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.filter(col, val)
    }

    /// `WHERE col <op> val` for arbitrary SQL operators (`>=`, `<`,
    /// `!=`, ...). No operator validation — pass-through to SQL.
    #[doc(alias = "db_where_op")]
    pub fn filter_op(mut self, col: impl IntoColumn, op: &str, val: impl IntoVal) -> Self {
        self.where_terms.push(WhereTerm::Op(
            col.col_name(),
            op.to_string(),
            val.into_val(),
        ));
        self
    }

    /// Laravel-shape alias for [`Self::filter_op`].
    #[doc(alias = "filter_op")]
    pub fn db_where_op(self, col: impl IntoColumn, op: &str, val: impl IntoVal) -> Self {
        self.filter_op(col, op, val)
    }

    /// `WHERE (... OR col = val)` — folds into the previous WHERE
    /// clause to form a disjunction. If there is no previous clause,
    /// the new equality stands alone.
    #[doc(alias = "or_where")]
    pub fn or_filter(mut self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        let new = WhereTerm::Eq(col.col_name(), val.into_val());
        match self.where_terms.last_mut() {
            Some(WhereTerm::Or(group)) => group.push(new),
            Some(_) => {
                // Pop the previous term and wrap both in an Or group.
                let last = self
                    .where_terms
                    .pop()
                    .expect("checked Some in match arm above");
                self.where_terms.push(WhereTerm::Or(vec![last, new]));
            }
            None => {
                // No prior clause — the disjunction reduces to the new
                // equality. Push as a plain Eq so the renderer doesn't
                // emit a dangling `()` wrapper.
                self.where_terms.push(new);
            }
        }
        self
    }

    /// Laravel-shape alias for [`Self::or_filter`].
    #[doc(alias = "or_filter")]
    pub fn or_where(self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.or_filter(col, val)
    }

    /// `WHERE NOT (col = val)`.
    #[doc(alias = "where_not")]
    pub fn filter_not(mut self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.where_terms.push(WhereTerm::Not(Box::new(WhereTerm::Eq(
            col.col_name(),
            val.into_val(),
        ))));
        self
    }

    /// Laravel-shape alias for [`Self::filter_not`].
    #[doc(alias = "filter_not")]
    pub fn where_not(self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.filter_not(col, val)
    }

    // ---- Set membership --------------------------------------------------

    /// `WHERE col IN (v1, v2, ...)`. Empty list renders as `1 = 0`
    /// (no rows match) so the SQL stays well-formed.
    #[doc(alias = "where_in")]
    pub fn filter_in<V, I>(mut self, col: impl IntoColumn, vals: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: IntoVal,
    {
        let v: Vec<Value> = vals.into_iter().map(|x| x.into_val()).collect();
        self.where_terms.push(WhereTerm::In(col.col_name(), v));
        self
    }

    /// Laravel-shape alias for [`Self::filter_in`].
    #[doc(alias = "filter_in")]
    pub fn where_in<V, I>(self, col: impl IntoColumn, vals: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: IntoVal,
    {
        self.filter_in(col, vals)
    }

    /// `WHERE col NOT IN (v1, v2, ...)`. Empty list renders as `1 = 1`
    /// (every row matches) so the SQL stays well-formed.
    #[doc(alias = "where_not_in")]
    pub fn filter_not_in<V, I>(mut self, col: impl IntoColumn, vals: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: IntoVal,
    {
        let v: Vec<Value> = vals.into_iter().map(|x| x.into_val()).collect();
        self.where_terms.push(WhereTerm::NotIn(col.col_name(), v));
        self
    }

    /// Laravel-shape alias for [`Self::filter_not_in`].
    #[doc(alias = "filter_not_in")]
    pub fn where_not_in<V, I>(self, col: impl IntoColumn, vals: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: IntoVal,
    {
        self.filter_not_in(col, vals)
    }

    // ---- Range -----------------------------------------------------------

    /// `WHERE col BETWEEN low AND high` (inclusive). Mirrors SQL's
    /// inclusive semantics.
    #[doc(alias = "where_between")]
    pub fn filter_between<V: IntoVal + Clone>(
        mut self,
        col: impl IntoColumn,
        range: RangeInclusive<V>,
    ) -> Self {
        let (a, b) = (
            range.start().clone().into_val(),
            range.end().clone().into_val(),
        );
        self.where_terms
            .push(WhereTerm::Between(col.col_name(), a, b));
        self
    }

    /// Laravel-shape alias for [`Self::filter_between`].
    #[doc(alias = "filter_between")]
    pub fn where_between<V: IntoVal + Clone>(
        self,
        col: impl IntoColumn,
        range: RangeInclusive<V>,
    ) -> Self {
        self.filter_between(col, range)
    }

    /// `WHERE col NOT BETWEEN low AND high` (inclusive bounds).
    #[doc(alias = "where_not_between")]
    pub fn filter_not_between<V: IntoVal + Clone>(
        mut self,
        col: impl IntoColumn,
        range: RangeInclusive<V>,
    ) -> Self {
        let (a, b) = (
            range.start().clone().into_val(),
            range.end().clone().into_val(),
        );
        self.where_terms
            .push(WhereTerm::NotBetween(col.col_name(), a, b));
        self
    }

    /// Laravel-shape alias for [`Self::filter_not_between`].
    #[doc(alias = "filter_not_between")]
    pub fn where_not_between<V: IntoVal + Clone>(
        self,
        col: impl IntoColumn,
        range: RangeInclusive<V>,
    ) -> Self {
        self.filter_not_between(col, range)
    }

    // ---- Null tests ------------------------------------------------------

    /// `WHERE col IS NULL`.
    #[doc(alias = "where_null")]
    pub fn filter_null(mut self, col: impl IntoColumn) -> Self {
        self.where_terms.push(WhereTerm::Null(col.col_name()));
        self
    }

    /// Laravel-shape alias for [`Self::filter_null`].
    #[doc(alias = "filter_null")]
    pub fn where_null(self, col: impl IntoColumn) -> Self {
        self.filter_null(col)
    }

    /// `WHERE col IS NOT NULL`.
    #[doc(alias = "where_not_null")]
    pub fn filter_not_null(mut self, col: impl IntoColumn) -> Self {
        self.where_terms.push(WhereTerm::NotNull(col.col_name()));
        self
    }

    /// Laravel-shape alias for [`Self::filter_not_null`].
    #[doc(alias = "filter_not_null")]
    pub fn where_not_null(self, col: impl IntoColumn) -> Self {
        self.filter_not_null(col)
    }

    // ---- LIKE ------------------------------------------------------------

    /// `WHERE col LIKE pattern`. Pattern is passed verbatim — escape
    /// `%` / `_` at call site if needed.
    #[doc(alias = "where_like")]
    pub fn filter_like(mut self, col: impl IntoColumn, pattern: impl Into<String>) -> Self {
        self.where_terms
            .push(WhereTerm::Like(col.col_name(), pattern.into()));
        self
    }

    /// Laravel-shape alias for [`Self::filter_like`].
    #[doc(alias = "filter_like")]
    pub fn where_like(self, col: impl IntoColumn, pattern: impl Into<String>) -> Self {
        self.filter_like(col, pattern)
    }

    /// `WHERE col NOT LIKE pattern`.
    #[doc(alias = "where_not_like")]
    pub fn filter_not_like(mut self, col: impl IntoColumn, pattern: impl Into<String>) -> Self {
        self.where_terms
            .push(WhereTerm::NotLike(col.col_name(), pattern.into()));
        self
    }

    /// Laravel-shape alias for [`Self::filter_not_like`].
    #[doc(alias = "filter_not_like")]
    pub fn where_not_like(self, col: impl IntoColumn, pattern: impl Into<String>) -> Self {
        self.filter_not_like(col, pattern)
    }

    // ---- Date / time parts -----------------------------------------------

    /// `WHERE DATE(col) = val`. Backend-specific: Postgres / MySQL /
    /// SQLite each use their native date-extraction function.
    #[doc(alias = "where_date")]
    pub fn filter_date(mut self, col: impl IntoColumn, val: NaiveDate) -> Self {
        self.where_terms.push(WhereTerm::DatePart(
            DatePart::Date,
            col.col_name(),
            Value::String(val.to_string()),
        ));
        self
    }

    /// Laravel-shape alias for [`Self::filter_date`].
    #[doc(alias = "filter_date")]
    pub fn where_date(self, col: impl IntoColumn, val: NaiveDate) -> Self {
        self.filter_date(col, val)
    }

    /// `WHERE EXTRACT(DAY FROM col) = val` (or backend equivalent).
    #[doc(alias = "where_day")]
    pub fn filter_day(mut self, col: impl IntoColumn, val: u32) -> Self {
        self.where_terms.push(WhereTerm::DatePart(
            DatePart::Day,
            col.col_name(),
            Value::Number(val.into()),
        ));
        self
    }

    /// Laravel-shape alias for [`Self::filter_day`].
    #[doc(alias = "filter_day")]
    pub fn where_day(self, col: impl IntoColumn, val: u32) -> Self {
        self.filter_day(col, val)
    }

    /// `WHERE EXTRACT(MONTH FROM col) = val` (or backend equivalent).
    #[doc(alias = "where_month")]
    pub fn filter_month(mut self, col: impl IntoColumn, val: u32) -> Self {
        self.where_terms.push(WhereTerm::DatePart(
            DatePart::Month,
            col.col_name(),
            Value::Number(val.into()),
        ));
        self
    }

    /// Laravel-shape alias for [`Self::filter_month`].
    #[doc(alias = "filter_month")]
    pub fn where_month(self, col: impl IntoColumn, val: u32) -> Self {
        self.filter_month(col, val)
    }

    /// `WHERE EXTRACT(YEAR FROM col) = val` (or backend equivalent).
    #[doc(alias = "where_year")]
    pub fn filter_year(mut self, col: impl IntoColumn, val: i32) -> Self {
        self.where_terms.push(WhereTerm::DatePart(
            DatePart::Year,
            col.col_name(),
            Value::Number(val.into()),
        ));
        self
    }

    /// Laravel-shape alias for [`Self::filter_year`].
    #[doc(alias = "filter_year")]
    pub fn where_year(self, col: impl IntoColumn, val: i32) -> Self {
        self.filter_year(col, val)
    }

    /// `WHERE TIME(col) = val` (or backend equivalent).
    #[doc(alias = "where_time")]
    pub fn filter_time(mut self, col: impl IntoColumn, val: NaiveTime) -> Self {
        self.where_terms.push(WhereTerm::DatePart(
            DatePart::Time,
            col.col_name(),
            Value::String(val.to_string()),
        ));
        self
    }

    /// Laravel-shape alias for [`Self::filter_time`].
    #[doc(alias = "filter_time")]
    pub fn where_time(self, col: impl IntoColumn, val: NaiveTime) -> Self {
        self.filter_time(col, val)
    }

    // ---- JSON ------------------------------------------------------------

    /// JSON containment — backend-specific: Postgres `col @> val`,
    /// MySQL `JSON_CONTAINS(col, val)`, SQLite falls back to substring
    /// search via `instr`.
    #[doc(alias = "where_json_contains")]
    pub fn filter_json_contains(mut self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.where_terms
            .push(WhereTerm::JsonContains(col.col_name(), val.into_val()));
        self
    }

    /// Laravel-shape alias for [`Self::filter_json_contains`].
    #[doc(alias = "filter_json_contains")]
    pub fn where_json_contains(self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.filter_json_contains(col, val)
    }

    /// JSON array length comparison — `WHERE JSON_LENGTH(col) <op> len`
    /// (or backend equivalent).
    #[doc(alias = "where_json_length")]
    pub fn filter_json_length(mut self, col: impl IntoColumn, op: &str, len: i64) -> Self {
        self.where_terms
            .push(WhereTerm::JsonLength(col.col_name(), op.to_string(), len));
        self
    }

    /// Laravel-shape alias for [`Self::filter_json_length`].
    #[doc(alias = "filter_json_length")]
    pub fn where_json_length(self, col: impl IntoColumn, op: &str, len: i64) -> Self {
        self.filter_json_length(col, op, len)
    }

    // ---- Column-to-column + raw -----------------------------------------

    /// `WHERE a = b` — compare two columns directly (no bind values).
    #[doc(alias = "where_column")]
    pub fn filter_column(mut self, a: impl IntoColumn, b: impl IntoColumn) -> Self {
        self.where_terms
            .push(WhereTerm::Column(a.col_name(), b.col_name()));
        self
    }

    /// Laravel-shape alias for [`Self::filter_column`].
    #[doc(alias = "filter_column")]
    pub fn where_column(self, a: impl IntoColumn, b: impl IntoColumn) -> Self {
        self.filter_column(a, b)
    }

    /// `WHERE <sql>` — raw SQL fragment with positional bindings. The
    /// caller is responsible for placeholder shape (`?` for SQLite /
    /// MySQL, `$N` for Postgres).
    ///
    /// # Security
    ///
    /// `sql` is interpolated verbatim into the query string; only the
    /// `bindings` Vec is parameterised. **Never pass user input as the
    /// SQL fragment** — concatenating a request value into the fragment
    /// is a SQL-injection vulnerability. Put user input in the
    /// `bindings` Vec and reference each bind by its placeholder.
    #[doc(alias = "where_raw")]
    pub fn filter_raw(mut self, sql: impl Into<String>, bindings: Vec<Value>) -> Self {
        self.where_terms.push(WhereTerm::Raw(sql.into(), bindings));
        self
    }

    /// Laravel-shape alias for [`Self::filter_raw`].
    #[doc(alias = "filter_raw")]
    pub fn where_raw(self, sql: impl Into<String>, bindings: Vec<Value>) -> Self {
        self.filter_raw(sql, bindings)
    }

    // ---- Ordering / grouping / limit ------------------------------------

    /// `ORDER BY col <dir>`.
    pub fn order_by(mut self, col: impl IntoColumn, dir: Direction) -> Self {
        self.orders.push(OrderTerm::Col(col.col_name(), dir));
        self
    }

    /// Shortcut for `order_by(col, Direction::Desc)`.
    pub fn order_by_desc(self, col: impl IntoColumn) -> Self {
        self.order_by(col, Direction::Desc)
    }

    /// Shortcut for `order_by(col, Direction::Asc)`.
    pub fn order_by_asc(self, col: impl IntoColumn) -> Self {
        self.order_by(col, Direction::Asc)
    }

    /// `ORDER BY <raw>` — pass through arbitrary expressions
    /// (`age * -1`, `CASE WHEN ...`).
    ///
    /// # Security
    ///
    /// `sql` is interpolated verbatim into the query. **Never pass user
    /// input here** — it is the same SQL-injection surface as
    /// [`filter_raw`](Self::filter_raw) without even the placeholder
    /// indirection. Hardcode the expression or build it from a known
    /// allowlist.
    pub fn order_by_raw(mut self, sql: impl Into<String>) -> Self {
        self.orders.push(OrderTerm::Raw(sql.into()));
        self
    }

    /// `ORDER BY RANDOM()` — useful for sampling. Each backend emits
    /// its own randomisation function via the internal `render_orders`
    /// helper.
    pub fn in_random_order(mut self) -> Self {
        self.orders.push(OrderTerm::Random);
        self
    }

    /// `GROUP BY col` — append to the GROUP BY list.
    pub fn group_by(mut self, col: impl IntoColumn) -> Self {
        self.group_by.push(col.col_name());
        self
    }

    /// `HAVING col = val` — equality filter on a grouped result.
    pub fn having(mut self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.having_terms
            .push(WhereTerm::Eq(col.col_name(), val.into_val()));
        self
    }

    /// `HAVING col <op> val` — arbitrary-operator filter on a grouped
    /// result.
    pub fn having_op(mut self, col: impl IntoColumn, op: &str, val: impl IntoVal) -> Self {
        self.having_terms.push(WhereTerm::Op(
            col.col_name(),
            op.to_string(),
            val.into_val(),
        ));
        self
    }

    /// `LIMIT n`.
    pub fn limit(mut self, n: u64) -> Self {
        self.limit = Some(n);
        self
    }

    /// `OFFSET n`.
    pub fn offset(mut self, n: u64) -> Self {
        self.offset = Some(n);
        self
    }

    /// Laravel-shape alias for [`Self::limit`].
    pub fn take(self, n: u64) -> Self {
        self.limit(n)
    }

    /// Laravel-shape alias for [`Self::offset`].
    pub fn skip(self, n: u64) -> Self {
        self.offset(n)
    }

    /// `SELECT DISTINCT` — applied at render time.
    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    /// Override the SELECT column list. By default the builder selects
    /// every column on the model (`SELECT *`).
    pub fn select<I, S>(mut self, cols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.select_cols = Some(cols.into_iter().map(|s| s.into()).collect());
        self
    }

    /// Append one column to the SELECT list. If no `select` has been
    /// called yet, this initialises the list with just `col`.
    pub fn add_select(mut self, col: impl Into<String>) -> Self {
        self.select_cols
            .get_or_insert_with(Vec::new)
            .push(col.into());
        self
    }

    /// Replace the SELECT column list with a raw SQL fragment
    /// (`COUNT(*) AS total`, `name, COUNT(role) OVER (...)`, ...).
    ///
    /// # Security
    ///
    /// `raw` is interpolated verbatim into the query. **Never pass
    /// user input here** — same SQL-injection surface as
    /// [`filter_raw`](Self::filter_raw) and [`order_by_raw`](Self::order_by_raw).
    /// Hardcode the expression or build it from a known allowlist.
    pub fn select_raw(mut self, raw: impl Into<String>) -> Self {
        self.select_raw = Some(raw.into());
        self
    }

    /// Append a `UNION` arm. The placeholder counter is threaded across
    /// arms so Postgres `$N` numbering stays monotonic.
    pub fn union(mut self, other: Self) -> Self {
        self.unions.push((Box::new(other), false));
        self
    }

    /// Append a `UNION ALL` arm (duplicates retained).
    pub fn union_all(mut self, other: Self) -> Self {
        self.unions.push((Box::new(other), true));
        self
    }

    /// Emit `SELECT ... FOR UPDATE` — an exclusive row lock that
    /// blocks other transactions from reading-with-lock or writing
    /// the same rows until the holding transaction commits.
    ///
    /// Maps to:
    /// - **Postgres**: `SELECT ... FOR UPDATE`
    /// - **MySQL**: `SELECT ... FOR UPDATE`
    /// - **SQLite**: no SQL emitted (SQLite has no row-level locking;
    ///   a one-shot `warn!` lands on the
    ///   `suprnova::eloquent::lock` target the first time per process)
    ///
    /// The lock is only meaningful **inside a transaction** — outside
    /// one, the lock releases at statement end and the call is
    /// effectively a no-op semantically (the SQL still emits). Pair
    /// with `DB::transaction(|tx| ...)`:
    ///
    /// ```ignore
    /// DB::transaction(|tx| async move {
    ///     let order = Order::query()
    ///         .filter("id", 42)
    ///         .lock_for_update()
    ///         .first_or_fail()
    ///         .with_tx(&tx)
    ///         .await?;
    ///     // Other transactions wanting to lock id=42 block here until commit.
    ///     Ok(())
    /// }).await?;
    /// ```
    pub fn lock_for_update(mut self) -> Self {
        self.lock_mode = LockMode::ForUpdate;
        self
    }

    /// Emit `SELECT ... FOR SHARE` (Postgres) /
    /// `SELECT ... LOCK IN SHARE MODE` (MySQL) — a shared read lock
    /// that allows other shared readers but blocks concurrent
    /// `FOR UPDATE` writers.
    ///
    /// Use this when you need a consistent snapshot of a row (e.g. an
    /// inventory check) without preventing other readers. For most
    /// "lock then write" flows reach for [`Self::lock_for_update`]
    /// instead — a shared lock would still let another reader-then-
    /// writer race you.
    ///
    /// SQLite emits no lock SQL (one-shot `warn!`); the call is a
    /// no-op there.
    pub fn shared_lock(mut self) -> Self {
        self.lock_mode = LockMode::Shared;
        self
    }

    /// Whether this builder will emit a locked SELECT (`FOR UPDATE` /
    /// `FOR SHARE`). Defense-in-depth signal for the read terminals:
    /// when set, the executor resolver must skip the `__read_replica__`
    /// step — Postgres hot-standbys reject locked SELECTs outright,
    /// and MySQL replicas accept them but the lock is local to the
    /// replica and useless.
    fn wants_primary_pool(&self) -> bool {
        !matches!(self.lock_mode, LockMode::None)
    }

    /// Override the model's static cast pipeline for this query. T7b
    /// consumes this; T5 just plumbs it through.
    pub fn with_casts(
        mut self,
        casts: HashMap<&'static str, std::sync::Arc<dyn crate::eloquent::casts::DynCast>>,
    ) -> Self {
        self.runtime_casts = casts;
        self
    }

    /// Skip a named global scope on this query (framework-internal
    /// soft-delete bypass tag). Phase 10C T4's typed
    /// [`Self::without_global_scope`] is the canonical surface for
    /// user-defined global scopes; this method exists only for the
    /// framework's own `with_trashed` / `only_trashed` machinery,
    /// which predates the typed registry and uses a string tag
    /// instead.
    ///
    /// **Not part of the public API.** End users go through the typed
    /// `without_global_scope::<S>()`. This is `pub` only because the
    /// `#[suprnova::model(soft_deletes)]` macro expands into user
    /// crates and needs to call it; the `__` prefix flags it as
    /// internal.
    #[doc(hidden)]
    pub fn __disable_named_scope(mut self, name: &'static str) -> Self {
        self.global_scopes_disabled.push(name);
        self
    }

    /// Append one global scope's `TypeId` to the per-builder
    /// exclusion mask, consulted by
    /// [`ScopeRegistry::apply_to`][reg_apply_to] when walking the
    /// per-model registry.
    ///
    /// **Not part of the public API.** This is `pub` only because the
    /// `#[suprnova::model]` macro emits the per-model static helper
    /// `Self::without_global_scope::<S>()` into user crates, and that
    /// helper needs to call this method to set the mask before the
    /// registry runs. The macro-emitted helper is the correct surface
    /// for end users — it constructs a fresh `Builder`, sets the mask,
    /// THEN runs the registry, so the opt-out actually lands.
    ///
    /// Chaining this method onto the builder returned by
    /// `Model::query()` is silently ineffective: `query()` applies
    /// registered scopes EAGERLY at construction time, so the scope
    /// has already mutated `where_terms` by the time
    /// `.without_global_scope::<S>()` adds the `TypeId` to the
    /// exclusion mask.
    ///
    /// Use the macro-emitted static helper instead:
    ///
    /// ```ignore
    /// // Constructs the builder, sets the mask, runs the registry —
    /// // opt-out lands.
    /// let everything = User::without_global_scope::<TenantScope>()
    ///     .get()
    ///     .await?;
    /// ```
    ///
    /// [reg_apply_to]: crate::eloquent::ScopeRegistry
    #[doc(hidden)]
    pub fn without_global_scope<S: 'static>(mut self) -> Self {
        self.excluded_scopes.push(std::any::TypeId::of::<S>());
        self
    }

    /// Set `skip_all_scopes = true`. Consulted by
    /// [`ScopeRegistry::apply_to`][reg_apply_to] to short-circuit
    /// every registered scope for this builder.
    ///
    /// **Not part of the public API.** Same rationale as
    /// [`Self::without_global_scope`]: this is `pub` only because the
    /// `#[suprnova::model]` macro emits the per-model static helper
    /// `Self::without_global_scopes()`, which needs to call this
    /// method to set the bypass flag before the registry runs.
    /// Chaining onto a builder returned by `Model::query()` is
    /// silently ineffective — scopes already ran.
    ///
    /// Use the macro-emitted static helper instead:
    ///
    /// ```ignore
    /// // Admin tooling: read every row.
    /// let everything = User::without_global_scopes()
    ///     .get()
    ///     .await?;
    /// ```
    ///
    /// [reg_apply_to]: crate::eloquent::ScopeRegistry
    #[doc(hidden)]
    pub fn without_global_scopes(mut self) -> Self {
        self.skip_all_scopes = true;
        self
    }
}

// ---- SQL rendering -- placeholder dialect --------------------------------

/// Per-backend placeholder convention. Postgres uses `$1`, `$2`, ...;
/// SQLite + MySQL use `?` everywhere.
fn placeholder(backend: DbBackend, n: usize) -> String {
    match backend {
        DbBackend::Postgres => format!("${n}"),
        _ => "?".to_string(),
    }
}

/// Render a date-extraction function for the backend.
fn render_date_part(backend: DbBackend, part: DatePart, col: &str) -> String {
    match (backend, part) {
        (DbBackend::Postgres, DatePart::Date) => format!("DATE({col})"),
        (DbBackend::Postgres, DatePart::Day) => format!("EXTRACT(DAY FROM {col})"),
        (DbBackend::Postgres, DatePart::Month) => format!("EXTRACT(MONTH FROM {col})"),
        (DbBackend::Postgres, DatePart::Year) => format!("EXTRACT(YEAR FROM {col})"),
        (DbBackend::Postgres, DatePart::Time) => format!("CAST({col} AS TIME)"),
        (DbBackend::MySql, DatePart::Date) => format!("DATE({col})"),
        (DbBackend::MySql, DatePart::Day) => format!("DAY({col})"),
        (DbBackend::MySql, DatePart::Month) => format!("MONTH({col})"),
        (DbBackend::MySql, DatePart::Year) => format!("YEAR({col})"),
        (DbBackend::MySql, DatePart::Time) => format!("TIME({col})"),
        (DbBackend::Sqlite, DatePart::Date) => format!("DATE({col})"),
        (DbBackend::Sqlite, DatePart::Day) => {
            format!("CAST(strftime('%d', {col}) AS INTEGER)")
        }
        (DbBackend::Sqlite, DatePart::Month) => {
            format!("CAST(strftime('%m', {col}) AS INTEGER)")
        }
        (DbBackend::Sqlite, DatePart::Year) => {
            format!("CAST(strftime('%Y', {col}) AS INTEGER)")
        }
        (DbBackend::Sqlite, DatePart::Time) => format!("strftime('%H:%M:%S', {col})"),
    }
}

/// Render an `EXISTS (...)` / `NOT EXISTS (...)` correlated subquery.
///
/// Three join shapes, dispatched on which slots the spec carries:
///
/// 1. **Pivot** (`pivot_table` populated) — m2m / morph-m2m / through:
///    ```sql
///    EXISTS (SELECT 1 FROM target
///             INNER JOIN pivot
///                ON pivot.<pivot_related_key> = target.<related_pk>
///             WHERE pivot.<pivot_parent_key> = parent.<parent_key>
///               AND <morph_type_column> = '<morph_type_value>'
///               AND <inner where terms>)
///    ```
/// 2. **BelongsTo** (`foreign_key` on parent table, `parent_key` is
///    target's PK) — child.id = parent.fk:
///    ```sql
///    EXISTS (SELECT 1 FROM target WHERE target.<parent_key> = parent.<foreign_key>)
///    ```
/// 3. **Has** (`foreign_key` on target table, `parent_key` is parent
///    PK) — child.fk = parent.pk:
///    ```sql
///    EXISTS (SELECT 1 FROM target WHERE target.<foreign_key> = parent.<parent_key>
///                                   AND <morph_type_column> = '<morph_type_value>'
///                                   AND <inner where terms>)
///    ```
///
/// When `spec.count_op`/`count_value` are set the renderer emits a
/// scalar `(SELECT COUNT(*) ...) <op> <n>` shape instead of bare
/// `EXISTS`. When `spec.positive` is `false` the whole result wraps in
/// `NOT (...)`.
///
/// The renderer threads `values` + `n` through the placeholder
/// counter — Postgres `$N` numbers stay monotonic across the parent's
/// WHERE clause and the subquery body, the same way UNION arms do.
fn render_exists(
    backend: DbBackend,
    spec: &ExistsSpec,
    values: &mut Vec<SeaValue>,
    n: &mut usize,
) -> String {
    let mut where_parts: Vec<String> = Vec::new();

    // Three shapes — pivot, belongs-to, has — selected by which slots
    // the spec carries. The renderer is intentionally explicit rather
    // than data-driven: each shape has different correlation logic, and
    // a generic templater would obscure which join lands on which side.
    let from_clause = if !spec.pivot_table.is_empty() {
        // Pivot path. `target_table` JOIN `pivot_table` on
        // `pivot.related_key = target.related_pk`. The correlation
        // back to the parent goes on `pivot.parent_key = parent.pk`.
        let join_clause = if spec.related_pk.is_empty() || spec.pivot_related_key.is_empty() {
            // Degenerate pivot (no target join column). Fall back to
            // the parent-correlation alone — degrades gracefully when
            // the macro could only supply the parent side.
            spec.pivot_table.clone()
        } else {
            format!(
                "{pivot} INNER JOIN {target} ON {pivot}.{prk} = {target}.{tpk}",
                pivot = spec.pivot_table,
                target = spec.target_table,
                prk = spec.pivot_related_key,
                tpk = spec.related_pk,
            )
        };
        where_parts.push(format!(
            "{pivot}.{ppk} = {parent}.{pk}",
            pivot = spec.pivot_table,
            ppk = spec.pivot_parent_key,
            parent = spec.parent_table,
            pk = spec.parent_key,
        ));
        if !spec.morph_type_column.is_empty() && !spec.morph_type_value.is_empty() {
            *n += 1;
            let ph = placeholder(backend, *n);
            values.push(SeaValue::String(Some(Box::new(
                spec.morph_type_value.clone(),
            ))));
            where_parts.push(format!(
                "{pivot}.{col} = {ph}",
                pivot = spec.pivot_table,
                col = spec.morph_type_column,
            ));
        }
        join_clause
    } else if !spec.target_table.is_empty() {
        // Has / belongs-to path. The correlation column on the target
        // side is `foreign_key`; on the parent side it's `parent_key`.
        where_parts.push(format!(
            "{target}.{fk} = {parent}.{pk}",
            target = spec.target_table,
            fk = spec.foreign_key,
            parent = spec.parent_table,
            pk = spec.parent_key,
        ));
        if !spec.morph_type_column.is_empty() && !spec.morph_type_value.is_empty() {
            *n += 1;
            let ph = placeholder(backend, *n);
            values.push(SeaValue::String(Some(Box::new(
                spec.morph_type_value.clone(),
            ))));
            where_parts.push(format!(
                "{target}.{col} = {ph}",
                target = spec.target_table,
                col = spec.morph_type_column,
            ));
        }
        spec.target_table.clone()
    } else {
        // No target table and no pivot — there's no SQL we can render.
        // This shouldn't be reachable from the builder API, but if
        // metadata is missing we render a clause that fails closed:
        // `EXISTS (SELECT 1 WHERE 1 = 0)` evaluates to FALSE, which
        // matches "you asked for related rows we can't locate, so
        // none qualify."
        if spec.positive {
            return "EXISTS (SELECT 1 WHERE 1 = 0)".to_string();
        } else {
            return "NOT EXISTS (SELECT 1 WHERE 1 = 0)".to_string();
        }
    };

    // Auto-apply the related model's soft-delete scope, so `has` /
    // `where_has` agrees with the related model's default scope.
    // Skip on the degenerate "no target table" path above (which
    // already returned 1 = 0); skip when the related model does not
    // soft-delete. Always qualify with the target table when present
    // so the column reads unambiguously inside the subquery.
    if !spec.related_soft_deletes_column.is_empty() && !spec.target_table.is_empty() {
        where_parts.push(format!(
            "{}.{} IS NULL",
            spec.target_table, spec.related_soft_deletes_column,
        ));
    }

    // Inner constraint from `where_has`'s closure.
    for t in &spec.inner_terms {
        let part = render_subquery_term(backend, t, values, n);
        where_parts.push(part);
    }

    // `where_relation` shortcut: col op val constraint inline.
    if let (Some(col), Some(val)) = (&spec.relation_column, &spec.relation_value) {
        let op = spec
            .relation_op
            .as_deref()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "=".to_string());
        *n += 1;
        let ph = placeholder(backend, *n);
        values.push(json_value_to_sea_value(val));
        // Qualify with the target table when present so the col reads
        // unambiguously in the subquery's WHERE — Laravel's
        // whereRelation always renders the qualified form.
        if !spec.target_table.is_empty() {
            where_parts.push(format!("{}.{} {} {}", spec.target_table, col, op, ph));
        } else {
            where_parts.push(format!("{col} {op} {ph}"));
        }
    }

    let where_sql = if where_parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", where_parts.join(" AND "))
    };

    let body = if let (Some(op), Some(count)) = (&spec.count_op, spec.count_value) {
        // Scalar-subquery form: `(SELECT COUNT(*) FROM ...) <op> <count>`.
        // The count value lands inline (not bound) because it's a
        // typed `i64` we control — no injection surface. The operator
        // already passed `validate_sql_operator`.
        format!(
            "(SELECT COUNT(*) FROM {from}{where_sql}) {op} {count}",
            from = from_clause,
            where_sql = where_sql,
        )
    } else {
        format!(
            "EXISTS (SELECT 1 FROM {from}{where_sql})",
            from = from_clause
        )
    };

    if spec.positive {
        body
    } else {
        // Count-mode and exists-mode wrap differently. For
        // `(SELECT COUNT(*) ...) op n`, polarity flips the operator
        // semantically; the simplest correct render is to wrap the
        // whole comparison in NOT. SQL's NOT against a comparison is
        // well-defined and matches Laravel's `orDoesntHave` behaviour.
        format!("NOT ({body})")
    }
}

/// Render a single [`WhereTerm`] inside the EXISTS subquery body.
/// Mirrors `Builder::render_where_term`'s arms but free-standing so the
/// renderer doesn't require a `Builder<M>` receiver — the inner terms
/// were copied off a typed `Builder<R>` at `where_has` time and the
/// types `R` have already been discarded.
fn render_subquery_term(
    backend: DbBackend,
    term: &WhereTerm,
    values: &mut Vec<SeaValue>,
    n: &mut usize,
) -> String {
    match term {
        WhereTerm::Eq(col, v) => {
            *n += 1;
            let ph = placeholder(backend, *n);
            values.push(json_value_to_sea_value(v));
            format!("{col} = {ph}")
        }
        WhereTerm::Op(col, op, v) => {
            *n += 1;
            let ph = placeholder(backend, *n);
            values.push(json_value_to_sea_value(v));
            format!("{col} {op} {ph}")
        }
        WhereTerm::In(col, vs) => {
            let phs: Vec<String> = vs
                .iter()
                .map(|v| {
                    *n += 1;
                    let ph = placeholder(backend, *n);
                    values.push(json_value_to_sea_value(v));
                    ph
                })
                .collect();
            if phs.is_empty() {
                "1 = 0".to_string()
            } else {
                format!("{col} IN ({})", phs.join(", "))
            }
        }
        WhereTerm::NotIn(col, vs) => {
            let phs: Vec<String> = vs
                .iter()
                .map(|v| {
                    *n += 1;
                    let ph = placeholder(backend, *n);
                    values.push(json_value_to_sea_value(v));
                    ph
                })
                .collect();
            if phs.is_empty() {
                "1 = 1".to_string()
            } else {
                format!("{col} NOT IN ({})", phs.join(", "))
            }
        }
        WhereTerm::Between(col, a, b) => {
            *n += 1;
            let pa = placeholder(backend, *n);
            values.push(json_value_to_sea_value(a));
            *n += 1;
            let pb = placeholder(backend, *n);
            values.push(json_value_to_sea_value(b));
            format!("{col} BETWEEN {pa} AND {pb}")
        }
        WhereTerm::NotBetween(col, a, b) => {
            *n += 1;
            let pa = placeholder(backend, *n);
            values.push(json_value_to_sea_value(a));
            *n += 1;
            let pb = placeholder(backend, *n);
            values.push(json_value_to_sea_value(b));
            format!("{col} NOT BETWEEN {pa} AND {pb}")
        }
        WhereTerm::Null(col) => format!("{col} IS NULL"),
        WhereTerm::NotNull(col) => format!("{col} IS NOT NULL"),
        WhereTerm::Like(col, pat) => {
            *n += 1;
            let ph = placeholder(backend, *n);
            values.push(SeaValue::String(Some(Box::new(pat.clone()))));
            format!("{col} LIKE {ph}")
        }
        WhereTerm::NotLike(col, pat) => {
            *n += 1;
            let ph = placeholder(backend, *n);
            values.push(SeaValue::String(Some(Box::new(pat.clone()))));
            format!("{col} NOT LIKE {ph}")
        }
        WhereTerm::Column(a, b) => format!("{a} = {b}"),
        WhereTerm::Raw(sql, bindings) => {
            for v in bindings {
                *n += 1;
                values.push(json_value_to_sea_value(v));
            }
            sql.clone()
        }
        WhereTerm::JsonContains(col, v) => {
            *n += 1;
            let ph = placeholder(backend, *n);
            values.push(json_value_to_sea_value(v));
            render_json_contains(backend, col, &ph)
        }
        WhereTerm::JsonLength(col, op, len) => render_json_length(backend, col, op, *len),
        WhereTerm::DatePart(part, col, v) => {
            *n += 1;
            let ph = placeholder(backend, *n);
            values.push(json_value_to_sea_value(v));
            let lhs = render_date_part(backend, *part, col);
            format!("{lhs} = {ph}")
        }
        WhereTerm::Not(inner) => {
            let inner_sql = render_subquery_term(backend, inner, values, n);
            format!("NOT ({inner_sql})")
        }
        WhereTerm::Or(terms) => {
            let parts: Vec<String> = terms
                .iter()
                .map(|t| render_subquery_term(backend, t, values, n))
                .collect();
            format!("({})", parts.join(" OR "))
        }
        WhereTerm::Exists(spec) => render_exists(backend, spec, values, n),
    }
}

fn render_json_contains(backend: DbBackend, col: &str, ph: &str) -> String {
    match backend {
        DbBackend::Postgres => format!("{col} @> {ph}"),
        DbBackend::MySql => format!("JSON_CONTAINS({col}, {ph})"),
        DbBackend::Sqlite => format!("instr({col}, {ph}) > 0"),
    }
}

fn render_json_length(backend: DbBackend, col: &str, op: &str, len: i64) -> String {
    match backend {
        DbBackend::Postgres => format!("jsonb_array_length({col}::jsonb) {op} {len}"),
        DbBackend::MySql => format!("JSON_LENGTH({col}) {op} {len}"),
        DbBackend::Sqlite => format!("json_array_length({col}) {op} {len}"),
    }
}

/// Phase 10C T9 — log a single `warn!` per process the first time a
/// SQLite query reaches `lock_for_update` / `shared_lock`. SQLite has
/// no row-level locking; the lock methods compile so cross-backend
/// code stays portable, but emitting the warning once per process
/// surfaces the no-op without spamming high-volume code paths.
fn warn_sqlite_lock_once() {
    use std::sync::Once;
    static WARN: Once = Once::new();
    WARN.call_once(|| {
        tracing::warn!(
            target: "suprnova::eloquent::lock",
            "lock_for_update / shared_lock are no-ops on SQLite; SQLite uses file-level transaction locking only"
        );
    });
}

impl<M> Builder<M> {
    fn render_where_term(
        &self,
        backend: DbBackend,
        term: &WhereTerm,
        values: &mut Vec<SeaValue>,
        n: &mut usize,
    ) -> String {
        match term {
            WhereTerm::Eq(col, v) => {
                *n += 1;
                let ph = placeholder(backend, *n);
                values.push(json_value_to_sea_value(v));
                format!("{col} = {ph}")
            }
            WhereTerm::Op(col, op, v) => {
                *n += 1;
                let ph = placeholder(backend, *n);
                values.push(json_value_to_sea_value(v));
                format!("{col} {op} {ph}")
            }
            WhereTerm::In(col, vs) => {
                let phs: Vec<String> = vs
                    .iter()
                    .map(|v| {
                        *n += 1;
                        let ph = placeholder(backend, *n);
                        values.push(json_value_to_sea_value(v));
                        ph
                    })
                    .collect();
                if phs.is_empty() {
                    "1 = 0".to_string()
                } else {
                    format!("{col} IN ({})", phs.join(", "))
                }
            }
            WhereTerm::NotIn(col, vs) => {
                let phs: Vec<String> = vs
                    .iter()
                    .map(|v| {
                        *n += 1;
                        let ph = placeholder(backend, *n);
                        values.push(json_value_to_sea_value(v));
                        ph
                    })
                    .collect();
                if phs.is_empty() {
                    "1 = 1".to_string()
                } else {
                    format!("{col} NOT IN ({})", phs.join(", "))
                }
            }
            WhereTerm::Between(col, a, b) => {
                *n += 1;
                let pa = placeholder(backend, *n);
                values.push(json_value_to_sea_value(a));
                *n += 1;
                let pb = placeholder(backend, *n);
                values.push(json_value_to_sea_value(b));
                format!("{col} BETWEEN {pa} AND {pb}")
            }
            WhereTerm::NotBetween(col, a, b) => {
                *n += 1;
                let pa = placeholder(backend, *n);
                values.push(json_value_to_sea_value(a));
                *n += 1;
                let pb = placeholder(backend, *n);
                values.push(json_value_to_sea_value(b));
                format!("{col} NOT BETWEEN {pa} AND {pb}")
            }
            WhereTerm::Null(col) => format!("{col} IS NULL"),
            WhereTerm::NotNull(col) => format!("{col} IS NOT NULL"),
            WhereTerm::Like(col, pat) => {
                *n += 1;
                let ph = placeholder(backend, *n);
                values.push(SeaValue::String(Some(Box::new(pat.clone()))));
                format!("{col} LIKE {ph}")
            }
            WhereTerm::NotLike(col, pat) => {
                *n += 1;
                let ph = placeholder(backend, *n);
                values.push(SeaValue::String(Some(Box::new(pat.clone()))));
                format!("{col} NOT LIKE {ph}")
            }
            WhereTerm::Column(a, b) => format!("{a} = {b}"),
            WhereTerm::Raw(sql, bindings) => {
                for v in bindings {
                    *n += 1;
                    values.push(json_value_to_sea_value(v));
                }
                sql.clone()
            }
            WhereTerm::JsonContains(col, v) => {
                *n += 1;
                let ph = placeholder(backend, *n);
                values.push(json_value_to_sea_value(v));
                render_json_contains(backend, col, &ph)
            }
            WhereTerm::JsonLength(col, op, len) => render_json_length(backend, col, op, *len),
            WhereTerm::DatePart(part, col, v) => {
                *n += 1;
                let ph = placeholder(backend, *n);
                values.push(json_value_to_sea_value(v));
                let lhs = render_date_part(backend, *part, col);
                format!("{lhs} = {ph}")
            }
            WhereTerm::Not(inner) => {
                let inner_sql = self.render_where_term(backend, inner, values, n);
                format!("NOT ({inner_sql})")
            }
            WhereTerm::Or(terms) => {
                let parts: Vec<String> = terms
                    .iter()
                    .map(|t| self.render_where_term(backend, t, values, n))
                    .collect();
                format!("({})", parts.join(" OR "))
            }
            WhereTerm::Exists(spec) => render_exists(backend, spec, values, n),
        }
    }

    fn render_orders(&self) -> String {
        if self.orders.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = self
            .orders
            .iter()
            .map(|o| match o {
                OrderTerm::Col(col, dir) => format!("{col} {}", dir.sql()),
                OrderTerm::Raw(sql) => sql.clone(),
                OrderTerm::Random => "RANDOM()".to_string(),
            })
            .collect();
        format!(" ORDER BY {}", parts.join(", "))
    }

    fn render_having(
        &self,
        backend: DbBackend,
        values: &mut Vec<SeaValue>,
        n: &mut usize,
    ) -> String {
        if self.having_terms.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = self
            .having_terms
            .iter()
            .map(|t| self.render_where_term(backend, t, values, n))
            .collect();
        format!(" HAVING {}", parts.join(" AND "))
    }

    pub(crate) fn render_select_for(
        &self,
        backend: DbBackend,
        table: &str,
        column_expr: &str,
    ) -> Result<(String, Vec<SeaValue>), FrameworkError> {
        // Audit HIGH `eloquent` #1: every identifier and operator on
        // this builder must clear `validate_identifier` /
        // `validate_sql_operator` before reaching the SQL renderer.
        // Raw-SQL escape hatches (`select_raw`, `WhereTerm::Raw`,
        // `OrderTerm::Raw`) are deliberately skipped — they exist
        // precisely so power users can opt past the validator.
        self.validate_inputs()?;
        let mut values: Vec<SeaValue> = Vec::new();
        let mut n = 0;
        let mut sql = self.render_select_into(backend, table, column_expr, &mut values, &mut n);
        // Phase 10C T9 — row-lock hint goes at the very end of the
        // compound statement, after every UNION arm and every
        // ORDER BY / LIMIT / OFFSET. The lock applies to the outer
        // SELECT, so emitting it inside `render_select_into` would
        // place it mid-statement on union arms — wrong shape.
        let lock_clause: &str = match (backend, self.lock_mode) {
            (_, LockMode::None) => "",
            (DbBackend::Postgres, LockMode::ForUpdate) => " FOR UPDATE",
            (DbBackend::Postgres, LockMode::Shared) => " FOR SHARE",
            (DbBackend::MySql, LockMode::ForUpdate) => " FOR UPDATE",
            (DbBackend::MySql, LockMode::Shared) => " LOCK IN SHARE MODE",
            (DbBackend::Sqlite, LockMode::ForUpdate | LockMode::Shared) => {
                warn_sqlite_lock_once();
                ""
            }
        };
        sql.push_str(lock_clause);
        Ok((sql, values))
    }

    /// Render a COUNT-shaped SELECT against this builder.
    ///
    /// Two shapes depending on the builder's structure:
    ///
    /// **Flat case** — no `GROUP BY`, no `UNION` arms:
    /// ```sql
    /// SELECT COUNT(*) AS count FROM t WHERE ... HAVING ...
    /// ```
    /// `ORDER BY` / `LIMIT` / `OFFSET` are stripped — they don't affect
    /// the count and `ORDER BY` over a bare aggregate is a SQL error
    /// in some dialects.
    ///
    /// **Grouped or union case** — `GROUP BY` non-empty OR unions present:
    /// ```sql
    /// SELECT COUNT(*) AS count FROM (
    ///     SELECT 1 FROM t WHERE ... GROUP BY ... HAVING ...
    ///     UNION ...
    /// ) AS __suprnova_paginate_subquery
    /// ```
    /// The subquery wrap is necessary because `SELECT COUNT(*) ...
    /// GROUP BY` returns one row per group (each row reporting the
    /// group's size), not the number of groups. Same fix Laravel
    /// applies via `Builder::getCountForPagination` and SeaORM's
    /// `PaginatorTrait::count`.
    ///
    /// Returns a `(sql, values)` pair that can be fed to
    /// `Statement::from_sql_and_values`.
    pub(crate) fn render_count_select_for(
        &self,
        backend: DbBackend,
        table: &str,
    ) -> Result<(String, Vec<SeaValue>), FrameworkError> {
        // Audit HIGH `eloquent` #1 — same identifier validation as
        // `render_select_for`. Count uses the same WHERE / GROUP BY /
        // HAVING clauses, so the attack surface is identical.
        self.validate_inputs()?;
        let mut values: Vec<SeaValue> = Vec::new();
        let mut n = 0;
        let mut sql = String::new();

        let needs_subquery_wrap = !self.group_by.is_empty() || !self.unions.is_empty();

        if needs_subquery_wrap {
            // Wrap: SELECT COUNT(*) AS count FROM (<inner>) AS sub.
            // The inner SELECT keeps every shape that affects which
            // rows the page-fetch will see (where / group / having /
            // unions) but projects a constant column so the wrapper's
            // COUNT counts distinct grouped/unioned rows.
            sql.push_str("SELECT COUNT(*) AS count FROM (");
            sql.push_str("SELECT 1 AS __paginate_marker FROM ");
            sql.push_str(table);
            self.render_count_body(backend, &mut sql, &mut values, &mut n);

            // Union arms — recurse with the same placeholder counter
            // so Postgres `$N` stays monotonic. Each arm projects the
            // same `1 AS __paginate_marker` column.
            for (other, all) in &self.unions {
                let connector = if *all { " UNION ALL " } else { " UNION " };
                sql.push_str(connector);
                sql.push_str("SELECT 1 AS __paginate_marker FROM ");
                sql.push_str(table);
                other.render_count_body(backend, &mut sql, &mut values, &mut n);
            }

            sql.push_str(") AS __suprnova_paginate_subquery");
        } else {
            sql.push_str("SELECT COUNT(*) AS count FROM ");
            sql.push_str(table);
            self.render_count_body(backend, &mut sql, &mut values, &mut n);
        }

        Ok((sql, values))
    }

    /// Append the WHERE / GROUP BY / HAVING clauses (without the
    /// leading SELECT or FROM) onto `sql`. Used by
    /// [`Self::render_count_select_for`] for both the flat and
    /// subquery-wrapped shapes — DRY-ing the clause emission across
    /// the two render paths.
    fn render_count_body(
        &self,
        backend: DbBackend,
        sql: &mut String,
        values: &mut Vec<SeaValue>,
        n: &mut usize,
    ) {
        if !self.where_terms.is_empty() {
            sql.push_str(" WHERE ");
            let parts: Vec<String> = self
                .where_terms
                .iter()
                .map(|t| self.render_where_term(backend, t, values, n))
                .collect();
            sql.push_str(&parts.join(" AND "));
        }

        if !self.group_by.is_empty() {
            sql.push_str(" GROUP BY ");
            sql.push_str(&self.group_by.join(", "));
        }

        sql.push_str(&self.render_having(backend, values, n));
    }

    /// Internal — render this Builder's SELECT body into a shared
    /// `values` + `n` counter. Used by [`Self::render_select_for`] (the
    /// top-level entry) and by union recursion: unions must share the
    /// placeholder counter so Postgres `$N` numbering stays monotonic
    /// across the combined statement. Without this, the inner SELECT's
    /// `$N` would restart at `$1` and collide with the outer's bound
    /// parameters.
    fn render_select_into(
        &self,
        backend: DbBackend,
        table: &str,
        column_expr: &str,
        values: &mut Vec<SeaValue>,
        n: &mut usize,
    ) -> String {
        let mut sql = String::new();

        sql.push_str("SELECT ");
        if self.distinct {
            sql.push_str("DISTINCT ");
        }
        if let Some(raw) = &self.select_raw {
            sql.push_str(raw);
        } else if let Some(cols) = &self.select_cols {
            sql.push_str(&cols.join(", "));
        } else {
            sql.push_str(column_expr);
        }
        sql.push_str(" FROM ");
        sql.push_str(table);

        if !self.where_terms.is_empty() {
            sql.push_str(" WHERE ");
            let parts: Vec<String> = self
                .where_terms
                .iter()
                .map(|t| self.render_where_term(backend, t, values, n))
                .collect();
            sql.push_str(&parts.join(" AND "));
        }

        if !self.group_by.is_empty() {
            sql.push_str(" GROUP BY ");
            sql.push_str(&self.group_by.join(", "));
        }

        sql.push_str(&self.render_having(backend, values, n));
        sql.push_str(&self.render_orders());

        if let Some(l) = self.limit {
            sql.push_str(&format!(" LIMIT {l}"));
        }
        if let Some(o) = self.offset {
            sql.push_str(&format!(" OFFSET {o}"));
        }

        // Unions — recurse into the same `values` / `n` so placeholder
        // numbers stay monotonic across the combined statement.
        // Without this, Postgres `$N` placeholders would restart at $1
        // on the inner SELECT, colliding with the outer's bound
        // parameters and silently corrupting query results.
        //
        // The inner SELECT is appended verbatim (no parens) because
        // SQLite rejects `UNION (SELECT ...)` while Postgres / MySQL
        // accept either form. Standard SQL doesn't require the parens.
        for (other, all) in &self.unions {
            let connector = if *all { " UNION ALL " } else { " UNION " };
            sql.push_str(connector);
            let other_sql = other.render_select_into(backend, table, column_expr, values, n);
            sql.push_str(&other_sql);
        }

        sql
    }
}

// The `M: Model` bound re-elaborates Model's own where-clause bounds
// because Rust's trait elaboration doesn't transitively propagate
// associated-type bounds from a supertrait's where clause to a
// subtrait's method bodies. Without these, `Self::TABLE` and
// `render_select_for` inside the method bodies fail to type-check
// against the same constraints `Model::query()` is declared with.
// Same pattern as `FirstOrCreate` in `model.rs` for the same reason.
impl<M: Model> Builder<M>
where
    M: From<<M::Entity as sea_orm::EntityTrait>::Model>,
    <M::Entity as sea_orm::EntityTrait>::Model: From<M>
        + sea_orm::IntoActiveModel<<M::Entity as sea_orm::EntityTrait>::ActiveModel>
        + serde::Serialize
        + Send
        + Sync,
    <M::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<M::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    // ---- Has / where-has existence engine (Laravel parity) ---------------
    //
    // These methods produce correlated `EXISTS (...)` / `NOT EXISTS
    // (...)` subqueries from a relation name. The relation lookup uses
    // [`crate::eloquent::relations::find_relation`] keyed on `M`'s
    // `TypeId`; unknown names render the safe "no rows match" form so
    // a typo silently returns an empty result set instead of leaking
    // a full-table scan.

    #[allow(clippy::too_many_arguments)]
    fn build_exists_spec_for(
        &self,
        relation: &str,
        positive: bool,
        count_op: Option<&str>,
        count_value: Option<i64>,
        inner_terms: Vec<WhereTerm>,
        relation_column: Option<String>,
        relation_op: Option<String>,
        relation_value: Option<Value>,
    ) -> ExistsSpec {
        let entry = crate::eloquent::relations::find_relation::<M>(relation);
        match entry {
            Some(e) => ExistsSpec {
                parent_table: M::TABLE.to_string(),
                parent_key: e.parent_key.to_string(),
                target_table: e.target_table.to_string(),
                foreign_key: e.foreign_key.to_string(),
                pivot_table: e.pivot_table.to_string(),
                pivot_parent_key: e.pivot_parent_key.to_string(),
                pivot_related_key: e.pivot_related_key.to_string(),
                related_pk: if e.pivot_table.is_empty() {
                    String::new()
                } else {
                    e.target_primary_key.to_string()
                },
                morph_type_column: e.morph_type_column.to_string(),
                morph_type_value: e.morph_type_value.to_string(),
                related_soft_deletes_column: e.related_soft_deletes_column.to_string(),
                positive,
                count_op: count_op.map(str::to_string),
                count_value,
                inner_terms,
                relation_column,
                relation_op,
                relation_value,
            },
            None => ExistsSpec {
                parent_table: M::TABLE.to_string(),
                parent_key: String::new(),
                target_table: String::new(),
                foreign_key: String::new(),
                pivot_table: String::new(),
                pivot_parent_key: String::new(),
                pivot_related_key: String::new(),
                related_pk: String::new(),
                morph_type_column: String::new(),
                morph_type_value: String::new(),
                related_soft_deletes_column: String::new(),
                positive,
                count_op: count_op.map(str::to_string),
                count_value,
                inner_terms,
                relation_column,
                relation_op,
                relation_value,
            },
        }
    }

    /// `WHERE EXISTS (SELECT 1 FROM related ...)` — restrict to rows
    /// whose `relation` returns at least one matching child.
    ///
    /// ```ignore
    /// // Users who have at least one post.
    /// let users = User::query().has("posts").get().await?;
    /// ```
    pub fn has(mut self, relation: &str) -> Self {
        let spec =
            self.build_exists_spec_for(relation, true, None, None, Vec::new(), None, None, None);
        self.where_terms.push(WhereTerm::Exists(Box::new(spec)));
        self
    }

    /// `WHERE (SELECT COUNT(*) FROM related ...) <op> <count>` — like
    /// [`Self::has`] but with a count comparator.
    ///
    /// ```ignore
    /// // Users with at least 3 published posts.
    /// let prolific = User::query().has_count("posts", ">=", 3).get().await?;
    /// ```
    pub fn has_count(mut self, relation: &str, op: &str, count: i64) -> Self {
        let spec = self.build_exists_spec_for(
            relation,
            true,
            Some(op),
            Some(count),
            Vec::new(),
            None,
            None,
            None,
        );
        self.where_terms.push(WhereTerm::Exists(Box::new(spec)));
        self
    }

    /// `OR EXISTS (...)` — disjunction form of [`Self::has`].
    pub fn or_has(mut self, relation: &str) -> Self {
        let spec =
            self.build_exists_spec_for(relation, true, None, None, Vec::new(), None, None, None);
        let new = WhereTerm::Exists(Box::new(spec));
        self.merge_or_term(new);
        self
    }

    /// `WHERE NOT EXISTS (SELECT 1 FROM related ...)` — restrict to
    /// rows whose `relation` returns no matching children.
    pub fn doesnt_have(mut self, relation: &str) -> Self {
        let spec =
            self.build_exists_spec_for(relation, false, None, None, Vec::new(), None, None, None);
        self.where_terms.push(WhereTerm::Exists(Box::new(spec)));
        self
    }

    /// `OR NOT EXISTS (...)` — disjunction form of [`Self::doesnt_have`].
    pub fn or_doesnt_have(mut self, relation: &str) -> Self {
        let spec =
            self.build_exists_spec_for(relation, false, None, None, Vec::new(), None, None, None);
        let new = WhereTerm::Exists(Box::new(spec));
        self.merge_or_term(new);
        self
    }

    /// `WHERE EXISTS (SELECT 1 FROM related WHERE <closure>)` — take a
    /// closure constraining the inner builder; the WHERE terms it
    /// produces land in the subquery's body.
    ///
    /// ```ignore
    /// let recent = User::query()
    ///     .where_has::<Post, _>("posts", |q| q.filter_op("created_at", ">=", "2026-01-01"))
    ///     .get()
    ///     .await?;
    /// ```
    pub fn where_has<R, F>(mut self, relation: &str, predicate: F) -> Self
    where
        R: 'static,
        F: FnOnce(Builder<R>) -> Builder<R>,
    {
        let inner = predicate(Builder::<R>::new());
        let spec = self.build_exists_spec_for(
            relation,
            true,
            None,
            None,
            inner.where_terms,
            None,
            None,
            None,
        );
        self.where_terms.push(WhereTerm::Exists(Box::new(spec)));
        self
    }

    /// `OR EXISTS (... <closure>)` — disjunction form of [`Self::where_has`].
    pub fn or_where_has<R, F>(mut self, relation: &str, predicate: F) -> Self
    where
        R: 'static,
        F: FnOnce(Builder<R>) -> Builder<R>,
    {
        let inner = predicate(Builder::<R>::new());
        let spec = self.build_exists_spec_for(
            relation,
            true,
            None,
            None,
            inner.where_terms,
            None,
            None,
            None,
        );
        let new = WhereTerm::Exists(Box::new(spec));
        self.merge_or_term(new);
        self
    }

    /// `WHERE NOT EXISTS (... <closure>)`. Negated form of [`Self::where_has`].
    pub fn where_doesnt_have<R, F>(mut self, relation: &str, predicate: F) -> Self
    where
        R: 'static,
        F: FnOnce(Builder<R>) -> Builder<R>,
    {
        let inner = predicate(Builder::<R>::new());
        let spec = self.build_exists_spec_for(
            relation,
            false,
            None,
            None,
            inner.where_terms,
            None,
            None,
            None,
        );
        self.where_terms.push(WhereTerm::Exists(Box::new(spec)));
        self
    }

    /// `OR NOT EXISTS (... <closure>)`.
    pub fn or_where_doesnt_have<R, F>(mut self, relation: &str, predicate: F) -> Self
    where
        R: 'static,
        F: FnOnce(Builder<R>) -> Builder<R>,
    {
        let inner = predicate(Builder::<R>::new());
        let spec = self.build_exists_spec_for(
            relation,
            false,
            None,
            None,
            inner.where_terms,
            None,
            None,
            None,
        );
        let new = WhereTerm::Exists(Box::new(spec));
        self.merge_or_term(new);
        self
    }

    /// Laravel's `whereRelation` shortcut. Equivalent to
    /// `where_has::<R, _>(rel, |q| q.filter(col, val))` without the
    /// typed closure — the column constraint renders inline in the
    /// EXISTS subquery body.
    pub fn where_relation(
        mut self,
        relation: &str,
        col: impl IntoColumn,
        val: impl IntoVal,
    ) -> Self {
        let spec = self.build_exists_spec_for(
            relation,
            true,
            None,
            None,
            Vec::new(),
            Some(col.col_name()),
            Some("=".to_string()),
            Some(val.into_val()),
        );
        self.where_terms.push(WhereTerm::Exists(Box::new(spec)));
        self
    }

    /// Like [`Self::where_relation`] but takes an explicit comparison
    /// operator (`>`, `<=`, `!=`, ...).
    pub fn where_relation_op(
        mut self,
        relation: &str,
        col: impl IntoColumn,
        op: &str,
        val: impl IntoVal,
    ) -> Self {
        let spec = self.build_exists_spec_for(
            relation,
            true,
            None,
            None,
            Vec::new(),
            Some(col.col_name()),
            Some(op.to_string()),
            Some(val.into_val()),
        );
        self.where_terms.push(WhereTerm::Exists(Box::new(spec)));
        self
    }

    /// `OR ...` form of [`Self::where_relation`].
    pub fn or_where_relation(
        mut self,
        relation: &str,
        col: impl IntoColumn,
        val: impl IntoVal,
    ) -> Self {
        let spec = self.build_exists_spec_for(
            relation,
            true,
            None,
            None,
            Vec::new(),
            Some(col.col_name()),
            Some("=".to_string()),
            Some(val.into_val()),
        );
        let new = WhereTerm::Exists(Box::new(spec));
        self.merge_or_term(new);
        self
    }

    /// Laravel's `whereBelongsTo($parent, "rel")` — restrict to rows
    /// whose `rel` belongs-to-relation matches the given parent row's
    /// PK. Renders a direct `WHERE child.<fk> = <parent_pk>` because
    /// the belongs-to FK lives on THIS table (no EXISTS needed).
    ///
    /// ```ignore
    /// let posts = Post::query().where_belongs_to("author", author.id).get().await?;
    /// ```
    pub fn where_belongs_to(mut self, relation: &str, parent_pk: impl IntoVal) -> Self {
        if let Some(e) = crate::eloquent::relations::find_relation::<M>(relation) {
            self.where_terms.push(WhereTerm::Eq(
                e.foreign_key.to_string(),
                parent_pk.into_val(),
            ));
        } else {
            self.where_terms
                .push(WhereTerm::Raw("1 = 0".to_string(), Vec::new()));
        }
        self
    }

    // ---- where_key / where_key_not / latest / oldest -------------------

    /// Laravel-shape PK filter — `WHERE pk = id`. Sugar over
    /// `filter(M::primary_key_name(), id)`.
    pub fn where_key(self, id: impl IntoVal) -> Self {
        let pk = M::primary_key_name();
        self.filter(pk, id)
    }

    /// Rust-idiomatic alias for [`Self::where_key`].
    pub fn filter_key(self, id: impl IntoVal) -> Self {
        self.where_key(id)
    }

    /// Laravel-shape PK exclusion — `WHERE pk <> id`. Sugar over
    /// `filter_op(M::primary_key_name(), "!=", id)`.
    pub fn where_key_not(self, id: impl IntoVal) -> Self {
        let pk = M::primary_key_name();
        self.filter_op(pk, "!=", id)
    }

    /// Rust-idiomatic alias for [`Self::where_key_not`].
    pub fn filter_key_not(self, id: impl IntoVal) -> Self {
        self.where_key_not(id)
    }

    /// Laravel-shape `latest()` — `ORDER BY <col> DESC`. Defaults to
    /// `"created_at"`; pass an explicit column to override.
    pub fn latest(self) -> Self {
        self.order_by("created_at", Direction::Desc)
    }

    /// Like [`Self::latest`] but uses the named column.
    pub fn latest_by(self, col: impl IntoColumn) -> Self {
        self.order_by(col, Direction::Desc)
    }

    /// Laravel-shape `oldest()` — `ORDER BY <col> ASC`. Defaults to
    /// `"created_at"`; pass an explicit column to override.
    pub fn oldest(self) -> Self {
        self.order_by("created_at", Direction::Asc)
    }

    /// Like [`Self::oldest`] but uses the named column.
    pub fn oldest_by(self, col: impl IntoColumn) -> Self {
        self.order_by(col, Direction::Asc)
    }

    // ---- without / with_only (eager-load opt-outs) ------------------------

    /// Remove `relations` from the eager-load plan. No-op when none of
    /// the names match. Mirrors Laravel's `Builder::without`.
    pub fn without<I, S>(mut self, relations: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let drop: std::collections::HashSet<String> =
            relations.into_iter().map(Into::into).collect();
        self.eager_specs.retain(|spec| match spec {
            EagerSpec::With(n)
            | EagerSpec::WithCount(n)
            | EagerSpec::WithSum(n, _)
            | EagerSpec::WithAvg(n, _)
            | EagerSpec::WithMin(n, _)
            | EagerSpec::WithMax(n, _)
            | EagerSpec::WithWhere(n, _) => !drop.contains(n),
        });
        self
    }

    /// Replace the eager-load plan with exactly `relations`. Mirrors
    /// Laravel's `Builder::withOnly`.
    pub fn with_only<I, S>(mut self, relations: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.eager_specs.clear();
        for r in relations {
            self.eager_specs.push(EagerSpec::With(r.into()));
        }
        self
    }

    // ---- qualify_column / qualify_columns ---------------------------------

    /// Return `M::TABLE.col` — the fully-qualified column name. Useful
    /// for joins where bare `col` would be ambiguous. Mirrors
    /// Laravel's `Model::qualifyColumn`.
    pub fn qualify_column(col: &str) -> String {
        format!("{}.{}", M::TABLE, col)
    }

    /// Qualify every column in `cols` against `M::TABLE`. Returns a
    /// new `Vec<String>`.
    pub fn qualify_columns<I, S>(cols: I) -> Vec<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        cols.into_iter()
            .map(|c| format!("{}.{}", M::TABLE, c.as_ref()))
            .collect()
    }

    /// Render the SQL for debugging. Uses the live DB connection's
    /// backend if one is initialised, otherwise falls back to SQLite
    /// shape so tests without a connection still get deterministic
    /// output. For explicit-dialect rendering use [`Self::to_sql_for`].
    pub fn to_sql(&self) -> String {
        self.to_sql_with_bindings().0
    }

    /// Render the SQL for the live DB connection's backend, returning
    /// both the SQL string and the bound values.
    ///
    /// **Panics** when this builder contains an identifier or
    /// operator that fails [`crate::database::validate_identifier`] /
    /// [`crate::database::validate_sql_operator`] — the same
    /// validation the execution path applies. The debug-only API
    /// keeps an infallible signature; the execution path
    /// ([`Self::get`] / [`Self::count`] / ...) surfaces the same
    /// condition as `Err(FrameworkError)` instead.
    pub fn to_sql_with_bindings(&self) -> (String, Vec<SeaValue>) {
        let backend = DB::connection()
            .ok()
            .map(|db| db.inner().get_database_backend())
            .unwrap_or(DbBackend::Sqlite);
        self.render_select_for(backend, M::TABLE, "*")
            .expect("to_sql_with_bindings: builder contains invalid identifier/operator")
    }

    /// Render the SQL for a specific dialect. Useful when debugging
    /// cross-database behaviour or when the live connection backend
    /// differs from the one you want to inspect.
    pub fn to_sql_for(&self, backend: DbBackend) -> String {
        self.to_sql_with_bindings_for(backend).0
    }

    /// Render the SQL for a specific dialect, returning both the SQL
    /// string and the bound values.
    pub fn to_sql_with_bindings_for(&self, backend: DbBackend) -> (String, Vec<SeaValue>) {
        self.render_select_for(backend, M::TABLE, "*")
            .expect("to_sql_with_bindings_for: builder contains invalid identifier/operator")
    }

    /// Phase 10C T14 — log the rendered SQL via `tracing` and return
    /// `self` so the call is chainable inside an existing builder
    /// pipeline. Interactive debugging aid only — never bake into
    /// production paths.
    ///
    /// Uses the live DB connection's backend if one is initialised
    /// (the dialect the actual query will run against), otherwise
    /// falls back to SQLite so tests without a live connection still
    /// emit deterministic output. Matches the dispatch logic of
    /// [`Self::to_sql_with_bindings`].
    ///
    /// Mirrors Laravel's `Builder::dump()`.
    ///
    /// ```rust,ignore
    /// User::query()
    ///     .filter("active", true)
    ///     .dump()              // logs: SELECT * FROM users WHERE ...
    ///     .order_by_desc("id")
    ///     .get()
    ///     .await?;
    /// ```
    pub fn dump(self) -> Self {
        let backend = DB::connection()
            .ok()
            .map(|db| db.inner().get_database_backend())
            .unwrap_or(DbBackend::Sqlite);
        match self.render_select_for(backend, M::TABLE, "*") {
            Ok((sql, _values)) => {
                tracing::info!(
                    target: "suprnova::eloquent::dump",
                    sql = %sql,
                    "query",
                );
            }
            Err(e) => {
                // Debug-only path — log the validation error instead
                // of panicking so the user can keep chaining and see
                // the structural issue. The execution path
                // (`get`/`count`/...) surfaces the same error as
                // `Err`.
                tracing::error!(
                    target: "suprnova::eloquent::dump",
                    error = %e,
                    "dump: builder contains invalid identifier/operator",
                );
            }
        }
        self
    }

    /// Phase 10C T14 — log the rendered SQL at `tracing::error!` and
    /// then **panic** with the SQL embedded in the panic message.
    /// Interactive debugging only — never bake into a production
    /// path.
    ///
    /// Mirrors Laravel's `Builder::dd()` ("dump-and-die").
    ///
    /// ```rust,ignore
    /// // Inspect the exact SQL Eloquent will emit, then halt.
    /// User::query().filter("active", true).dd();
    /// ```
    ///
    /// Panics with `eloquent dd: <sql>` — the literal `eloquent dd`
    /// prefix is part of the contract so `#[should_panic(expected =
    /// "eloquent dd")]` test assertions stay stable.
    pub fn dd(self) -> ! {
        let backend = DB::connection()
            .ok()
            .map(|db| db.inner().get_database_backend())
            .unwrap_or(DbBackend::Sqlite);
        let sql = self
            .render_select_for(backend, M::TABLE, "*")
            .map(|(sql, _values)| sql)
            .unwrap_or_else(|e| format!("<invalid: {e}>"));
        tracing::error!(
            target: "suprnova::eloquent::dump",
            sql = %sql,
            "query (dd halt)",
        );
        panic!("eloquent dd: {sql}");
    }

    /// Render `DELETE FROM table WHERE ...` from the same WhereTerm
    /// AST. Consumed by Task 10's MassPrunable bulk-delete runner and
    /// any future path that needs an atomic delete from a Builder
    /// chain. Ignores select / order / group / having / limit / offset
    /// / unions — only the WHERE clauses apply to a DELETE.
    pub fn to_delete_sql_with_bindings_for(
        &self,
        backend: DbBackend,
        table: &str,
    ) -> (String, Vec<SeaValue>) {
        let mut sql = String::new();
        let mut values: Vec<SeaValue> = Vec::new();
        let mut n = 0;

        sql.push_str("DELETE FROM ");
        sql.push_str(table);

        if !self.where_terms.is_empty() {
            sql.push_str(" WHERE ");
            let parts: Vec<String> = self
                .where_terms
                .iter()
                .map(|t| self.render_where_term(backend, t, &mut values, &mut n))
                .collect();
            sql.push_str(&parts.join(" AND "));
        }

        (sql, values)
    }
}

// ---- Terminals -----------------------------------------------------------

// The `FromQueryResult` bound belongs on the entity's `Model` type —
// the SeaORM-generated `<M::Entity as EntityTrait>::Model` that
// DeriveEntityModel auto-implements FromQueryResult on. The user's
// struct `M` does NOT have FromQueryResult; we fetch into the entity's
// Model and convert via `M::from(row)`.
impl<M: Model> Builder<M>
where
    M: From<<M::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <M::Entity as sea_orm::EntityTrait>::Model: From<M>
        + sea_orm::IntoActiveModel<<M::Entity as sea_orm::EntityTrait>::ActiveModel>
        + FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <M::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<M::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Centralised read-side executor resolution. Routes through
    /// [`ExecutorChoice::resolve_read_avoid_replica`](crate::database::transaction::ExecutorChoice::resolve_read_avoid_replica)
    /// when the builder carries a lock mode and through the standard
    /// [`ExecutorChoice::resolve_read`](crate::database::transaction::ExecutorChoice::resolve_read)
    /// otherwise — every read terminal that emits SQL routes through
    /// this single point so the lock-vs-replica policy stays in one
    /// place.
    async fn resolve_read_executor(
        &self,
    ) -> Result<crate::database::transaction::ExecutorChoice, FrameworkError> {
        if self.wants_primary_pool() {
            crate::database::transaction::ExecutorChoice::resolve_read_avoid_replica(
                self.tx_override.as_ref(),
                self.connection_override.as_deref(),
                M::default_connection_name(),
            )
            .await
        } else {
            crate::database::transaction::ExecutorChoice::resolve_read(
                self.tx_override.as_ref(),
                self.connection_override.as_deref(),
                M::default_connection_name(),
            )
            .await
        }
    }

    /// Execute the SELECT and return every row.
    ///
    /// ## Fast path vs slow path
    ///
    /// When [`with_casts`] has NOT been called (`runtime_casts` empty)
    /// — the common case — rows are materialised through the
    /// macro-emitted `From<inner::Model> for M` impl, which routes
    /// through any *static* casts declared via `#[model(casts =
    /// { ... })]`. This is one allocation per row.
    ///
    /// When `runtime_casts` has entries, the static cast pipeline is
    /// bypassed entirely for this query — runtime casts are an
    /// *override*, not an addition. For each row the framework
    /// serialises the storage-shape inner Model directly to JSON,
    /// routes each listed column through the runtime cast's
    /// `DynCast::from_storage_json`, and deserialises the result
    /// straight into `M`. Columns with no runtime cast entry land in
    /// `M` in their raw storage shape — so a runtime override is
    /// expected to specify every column that needs coercion.
    ///
    /// ## Eager loading
    ///
    /// When [`with`] entries are present in the builder, the base
    /// SELECT runs first; each eager spec then triggers a call into
    /// the model's `__eager_load` dispatcher (via
    /// [`EagerLoadDispatch::eager_load`]) which issues per-relation
    /// IN-queries and populates each row's `__eager` cache. The
    /// `<rel>_loaded()` accessor emitted per relation then reads from
    /// that cache. T9 will extend this with `with_count` /
    /// `with_sum`-`max` / nested-path resolution.
    ///
    /// [`with_casts`]: Self::with_casts
    /// [`with`]: Self::with
    /// [`EagerLoadDispatch::eager_load`]: crate::eloquent::EagerLoadDispatch::eager_load
    ///
    /// ## Return type
    ///
    /// Phase 10C T5b returns a
    /// [`Collection<M>`](crate::eloquent::Collection), the Eloquent
    /// wrapper around `Vec<M>`. Slice-shape access (`.iter()`,
    /// `.len()`, indexing, `for row in &result`) works directly via
    /// `Deref<Target = [M]>`; call sites that need owned `Vec<M>` use
    /// `.into_vec()`. The model-aware surface (`pluck("col")`,
    /// `group_by("col")`, `sort_by("col")`, `sum::<T>("col")`, ...)
    /// composes on top.
    pub async fn get(mut self) -> Result<Collection<M>, FrameworkError> {
        // Phase 10C T1 — Retrieving fires ONCE per query (not per
        // row) before any SQL runs. Aligns with Laravel's
        // `retrieving` hook, which fires "just before a model is
        // hydrated from DB" once for the query as a whole.
        M::__dispatch_retrieving().await?;

        // Phase 10C T11/T12 — resolve executor with five-step precedence:
        // explicit `with_tx` override > ambient `CURRENT_TX` >
        // builder `on(name)` > per-model default conn >
        // `__read_replica__` auto-routing > default pool.
        let exec = self.resolve_read_executor().await?;
        let backend = exec.backend();
        let runtime_casts = self.runtime_casts.clone();
        // Move the eager plan out of `self` — `EagerSpec::WithWhere`
        // owns a `Box<dyn Any>` (the type-erased predicate) which is
        // not `Clone`. The base SELECT consumes `self`'s WHERE / ORDER
        // / LIMIT terms; afterwards we hand the plan to the eager
        // orchestrator.
        let eager_specs = std::mem::take(&mut self.eager_specs);
        let (sql, vals) = self.render_select_for(backend, M::TABLE, "*")?;
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);

        // Fetch into the entity's `Model` — the SeaORM type that's
        // auto-implementing `FromQueryResult`. This is the storage-shape
        // type, not the user's runtime struct. T11: route through the
        // resolved executor (transaction or pool).
        //
        // SeaORM's `.all<C: ConnectionTrait>` is generic + `Sized`;
        // `&dyn ConnectionTrait` won't satisfy it. Match the executor
        // variant and call `.all(...)` on each concrete arm so the
        // generic resolves to either `DatabaseTransaction` or
        // `DatabaseConnection`.
        let raw_rows = match &exec {
            crate::database::transaction::ExecutorChoice::Tx(t, _) => {
                <<M as EloquentModel>::Entity as sea_orm::EntityTrait>::Model::find_by_statement(
                    stmt,
                )
                .all(t.as_ref())
                .await
            }
            crate::database::transaction::ExecutorChoice::Pool(c, _) => {
                <<M as EloquentModel>::Entity as sea_orm::EntityTrait>::Model::find_by_statement(
                    stmt,
                )
                .all(c.inner())
                .await
            }
        }
        .map_err(|e| FrameworkError::database(e.to_string()))?;

        let mut out: Vec<M> = if runtime_casts.is_empty() {
            // Fast path — convert each row via the macro-emitted
            // fallible `Model::try_from_storage`, so a cast that fails
            // to decode a stored value propagates as a `FrameworkError`
            // instead of panicking.
            raw_rows
                .into_iter()
                .map(M::try_from_storage)
                .collect::<Result<Vec<_>, _>>()?
        } else {
            // Slow path (override mode) — serialise the storage-shape
            // row to JSON, apply each runtime cast in place, then
            // deserialise into M. Static casts on M are NOT applied;
            // the runtime cast map is treated as a full replacement
            // for this query.
            let mut buf = Vec::with_capacity(raw_rows.len());
            for row in raw_rows {
                let mut as_json = serde_json::to_value(&row).map_err(|e| {
                    FrameworkError::database(format!("serialise inner Model for runtime cast: {e}"))
                })?;
                if let serde_json::Value::Object(ref mut map) = as_json {
                    for (col, cast) in &runtime_casts {
                        if let Some(v) = map.get(*col).cloned() {
                            let coerced = cast.from_storage_json(&v)?;
                            map.insert((*col).to_string(), coerced);
                        }
                    }
                }
                let coerced_model: M = serde_json::from_value(as_json).map_err(|e| {
                    FrameworkError::database(format!("rehydrate model after runtime cast: {e}"))
                })?;
                buf.push(coerced_model);
            }
            buf
        };

        // T9 — eager loading. After the base SELECT lands, the
        // orchestrator walks each recorded `EagerSpec` and dispatches
        // into the per-model `__eager_load` /
        // `__count_relation` / `__aggregate_relation` (via the
        // `EagerLoadDispatch` trait the macro emits). Each call
        // mutates every row's `__eager` cache in-place. Nested
        // `"posts.comments"` paths recurse via
        // `__recurse_eager_load`.
        //
        // T11: `EagerLoadDispatch` takes `&DatabaseConnection` (concrete,
        // emitted by the macro across every relation kind). The `db`
        // parameter is retained for trait-signature stability — the
        // actual routing happens at each SQL leaf (`belongs_to_many.rs`
        // etc.) via `ExecutorChoice::resolve()`, which consults
        // `CURRENT_TX` and routes through the active transaction when
        // present. Outside a tx, leaves use the same pool we pass here;
        // inside a tx, this `db` is effectively ignored.
        if !eager_specs.is_empty() && !out.is_empty() {
            let eager_db = crate::eloquent::relations::eager::resolve_eager_connection(
                self.tx_override.as_ref(),
                self.connection_override.as_deref(),
                M::default_connection_name(),
            )
            .await?;
            crate::eloquent::relations::eager::apply_eager_specs::<M>(
                &mut out,
                eager_specs,
                eager_db.inner(),
            )
            .await?;
        }

        // Phase 10C T1 — Retrieved fires ONCE per hydrated row, AFTER
        // eager loads land. Listeners observe the fully-populated
        // model (relations cache + all hydrated columns), not the
        // partial post-SELECT shape.
        for row in &out {
            M::__dispatch_retrieved(row).await?;
        }

        Ok(Collection::from_vec(out))
    }

    /// Execute the SELECT and return at most one row.
    ///
    /// Dispatches `Retrieving` once before the SELECT and
    /// `Retrieved` once for the returned row (no dispatch when the
    /// query matches zero rows). Internally delegates to
    /// [`Self::get`] with `limit = 1`, which is where the event
    /// hooks fire — so `first` shares the same per-row dispatch
    /// contract.
    pub async fn first(mut self) -> Result<Option<M>, FrameworkError> {
        self.limit = Some(1);
        // `Collection<M>` derefs to `&[M]` but offers no owning `pop` —
        // unwrap to the inner `Vec` first.
        Ok(self.get().await?.into_vec().pop())
    }

    /// Execute the SELECT and return one row. Errors with
    /// `FrameworkError::ModelNotFound` (HTTP 404) if no row matches.
    /// Event-dispatch contract identical to [`Self::first`].
    pub async fn first_or_fail(self) -> Result<M, FrameworkError> {
        self.first()
            .await?
            .ok_or_else(|| FrameworkError::not_found("no rows matched"))
    }

    /// Laravel-shape `sole()` — succeed only when the query matches
    /// EXACTLY one row.
    ///
    /// Returns `FrameworkError::not_found` when zero rows match and
    /// `FrameworkError::bad_request` ("multiple rows matched") when two
    /// or more rows match. Useful in invariant-checking code paths
    /// where ambiguity is a bug, not a UX choice.
    pub async fn sole(mut self) -> Result<M, FrameworkError> {
        // Fetch up to 2 rows so we can distinguish the "exactly one"
        // case from the "many" case without paying a separate COUNT.
        self.limit = Some(2);
        let mut rows = self.get().await?.into_vec();
        match rows.len() {
            0 => Err(FrameworkError::not_found("no rows matched")),
            1 => Ok(rows.pop().expect("rows.len() == 1 checked above")),
            _ => Err(FrameworkError::bad_request(
                "multiple rows matched a sole() query",
            )),
        }
    }

    /// Laravel-shape `soleValue($col)` — fetch a single value, succeed
    /// only when one row matches. Variant of [`Self::sole`] that
    /// projects a column.
    pub async fn sole_value<T: TryGetable>(
        mut self,
        col: impl IntoColumn,
    ) -> Result<T, FrameworkError> {
        // T11/T12: respect `with_tx` + ambient CURRENT_TX + `on(name)`
        // + per-model default + `__read_replica__`.
        self.limit = Some(2);
        let exec = self.resolve_read_executor().await?;
        let backend = exec.backend();
        let col_name = col.col_name();
        let (sql, vals) = self.render_select_for(backend, M::TABLE, &col_name)?;
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let rows = exec
            .query_all(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        match rows.len() {
            0 => Err(FrameworkError::not_found("no rows matched")),
            1 => rows[0]
                .try_get::<T>("", &col_name)
                .map_err(|e| FrameworkError::database(e.to_string())),
            _ => Err(FrameworkError::bad_request(
                "multiple rows matched a sole_value() query",
            )),
        }
    }

    /// Laravel-shape `valueOrFail($col)` — fetch a single column from
    /// the first matching row, error when no row matches.
    pub async fn value_or_fail<T: TryGetable>(
        self,
        col: impl IntoColumn,
    ) -> Result<T, FrameworkError> {
        let col_name_owned = col.col_name();
        match self.value::<T>(col_name_owned.as_str()).await? {
            Some(v) => Ok(v),
            None => Err(FrameworkError::not_found(format!(
                "no value for column {col_name_owned}"
            ))),
        }
    }

    /// Whether the query matches at least one row.
    pub async fn exists(self) -> Result<bool, FrameworkError> {
        Ok(self.first().await?.is_some())
    }

    /// Whether the query matches zero rows.
    pub async fn doesnt_exist(self) -> Result<bool, FrameworkError> {
        Ok(!self.exists().await?)
    }

    /// `SELECT COUNT(*) FROM ...`.
    pub async fn count(self) -> Result<i64, FrameworkError> {
        self.aggregate_value::<i64>("COUNT(*)").await
    }

    /// Length-aware paginate. Runs a `COUNT(*)` query alongside the
    /// `LIMIT`/`OFFSET` SELECT — two queries per page.
    ///
    /// Reads the current page number from the request's `?page=N`
    /// query parameter (via [`crate::context::Context::query_param`]).
    /// Use [`Self::paginate_using`] to override the parameter name —
    /// useful when a page has multiple paginators that each need their
    /// own query string.
    ///
    /// Returns a [`LengthAwarePaginator<M>`](crate::LengthAwarePaginator) whose JSON shape matches
    /// Laravel's `LengthAwarePaginator::toArray()` — ready to ship to
    /// Inertia / JSON consumers without reshaping.
    ///
    /// ## Errors
    ///
    /// - `per_page == 0` → `FrameworkError::param("per_page")` (HTTP
    ///   400).
    /// - Any database error from the underlying COUNT or LIMIT/OFFSET
    ///   queries → `FrameworkError::Database`.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let users = User::query()
    ///     .filter("active", true)
    ///     .order_by_desc("created_at")
    ///     .paginate(20)
    ///     .await?;
    /// // users.data: Vec<User>, users.total: u64, users.last_page: u64, ...
    /// ```
    pub async fn paginate(
        self,
        per_page: u64,
    ) -> Result<crate::pagination::LengthAwarePaginator<M>, FrameworkError> {
        self.paginate_using("page", per_page).await
    }

    /// Length-aware paginate with a custom page-param name. See
    /// [`Self::paginate`] for the JSON shape and error semantics.
    ///
    /// `page_param` is the query-string key read for the current page
    /// number — e.g. `paginate_using("p", 20)` reads `?p=2`. Useful
    /// when a single page renders multiple independent paginators, so
    /// each can take a different query parameter:
    ///
    /// ```ignore
    /// let posts = Post::query().paginate_using("posts_page", 10).await?;
    /// let comments = Comment::query().paginate_using("comments_page", 25).await?;
    /// ```
    pub async fn paginate_using(
        self,
        page_param: &str,
        per_page: u64,
    ) -> Result<crate::pagination::LengthAwarePaginator<M>, FrameworkError> {
        if per_page == 0 {
            return Err(FrameworkError::param("per_page"));
        }
        let page = current_page_from_request(page_param);
        let offset = page.saturating_sub(1).saturating_mul(per_page);

        // Count phase — borrows `self`, doesn't consume it.
        // T11/T12: route through ExecutorChoice (with tx override +
        // connection override) so the COUNT runs against the same
        // executor as the page query.
        let exec = self.resolve_read_executor().await?;
        let backend = exec.backend();
        let (count_sql, count_vals) = self.render_count_select_for(backend, M::TABLE)?;
        let count_stmt = Statement::from_sql_and_values(backend, &count_sql, count_vals);
        let count_row = exec
            .query_one(count_stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let total: u64 = count_row
            .and_then(|r| r.try_get::<i64>("", "count").ok())
            .map(|n| n.max(0) as u64)
            .unwrap_or(0);

        // Page phase — consumes `self` for the actual fetch.
        let rows: Vec<M> = self.limit(per_page).offset(offset).get().await?.into_vec();

        let from = if rows.is_empty() {
            None
        } else {
            Some(offset + 1)
        };
        let to = if rows.is_empty() {
            None
        } else {
            Some(offset + rows.len() as u64)
        };

        Ok(crate::pagination::LengthAwarePaginator::with_window(
            rows, total, per_page, page, from, to,
        )
        .with_page_name(page_param))
    }

    /// Simple paginate — no COUNT query.
    ///
    /// Fetches `per_page + 1` rows; if the extra row exists, `has_more`
    /// is set and the row is trimmed from `data`. One query per page —
    /// cheap to compute for large tables where a total row count would
    /// be too expensive.
    ///
    /// Reads the current page from `?page=N` like [`Self::paginate`].
    ///
    /// ## Errors
    ///
    /// - `per_page == 0` → `FrameworkError::param("per_page")` (400).
    pub async fn simple_paginate(
        self,
        per_page: u64,
    ) -> Result<crate::pagination::Paginator<M>, FrameworkError> {
        if per_page == 0 {
            return Err(FrameworkError::param("per_page"));
        }
        let page = current_page_from_request("page");
        let offset = page.saturating_sub(1).saturating_mul(per_page);
        let raw: Vec<M> = self
            .limit(per_page + 1)
            .offset(offset)
            .get()
            .await?
            .into_vec();
        let has_more = (raw.len() as u64) > per_page;
        let mut rows = raw;
        if has_more {
            rows.truncate(per_page as usize);
        }
        Ok(crate::pagination::Paginator::new(
            rows, page, per_page, has_more,
        ))
    }

    /// Cursor paginate — opaque-cursor keyset pagination over the
    /// model's primary key.
    ///
    /// Bidirectional, matching Laravel's `cursorPaginate()` and the
    /// [`Pagination::cursor`](crate::pagination::Pagination::cursor)
    /// facade. Reads the current cursor from `?cursor=<opaque>`, decodes
    /// its direction, and walks forward (`pk > boundary`, ASC) or
    /// backward (`pk < boundary`, DESC then reversed to ASC). Emits both
    /// `next_cursor` and `prev_cursor` as the page's neighbours exist, so
    /// a client can page in either direction. Cursors are encrypted+MACd
    /// via `CursorPaginator::encode_value` so they can't be forged.
    ///
    /// Any existing `ORDER BY` on the builder is replaced — cursor
    /// pagination requires a stable total order over the PK.
    ///
    /// ## Errors
    ///
    /// - `per_page == 0` → `FrameworkError::param("per_page")` (400).
    /// - Invalid / tampered cursor → `FrameworkError::ParamParse`.
    pub async fn cursor_paginate(
        self,
        per_page: u64,
    ) -> Result<crate::pagination::CursorPaginator<M>, FrameworkError> {
        if per_page == 0 {
            return Err(FrameworkError::param("per_page"));
        }

        let pk = M::primary_key_name();
        let decoded = match current_cursor_from_request() {
            Some(c) => Some(crate::pagination::CursorPaginator::<M>::decode_value(&c)?),
            None => None,
        };
        let plan = crate::pagination::cursor::plan_scan(decoded);

        // Replace any existing ORDER BY with a stable PK sort in the
        // plan's direction — cursor pagination requires a total order
        // over the keyset column.
        let mut q = self;
        q.orders.clear();
        let mut q = if plan.order_asc {
            q.order_by_asc(pk)
        } else {
            q.order_by_desc(pk)
        };
        if let Some((op, boundary)) = &plan.filter {
            // Convert the typed boundary back to JSON; the builder's
            // `filter_op` pipeline rebinds it via `json_value_to_sea_value`
            // in the renderer. Every PK variant we care about (Int /
            // BigInt / Uuid / String) round-trips losslessly.
            let boundary_json = crate::eloquent::model::sea_value_to_json_loose(boundary);
            q = q.filter_op(pk, op, boundary_json);
        }

        let mut rows: Vec<M> = q.limit(per_page + 1).get().await?.into_vec();
        // Normalize a backward (DESC) scan back to ASC so finalize_page
        // sees the overflow row at the start.
        if !plan.order_asc {
            rows.reverse();
        }
        let (rows, flags) = crate::pagination::cursor::finalize_page(rows, per_page, &plan);

        let next_cursor = if flags.has_next && !rows.is_empty() {
            let pk_val: sea_orm::Value = rows.last().unwrap().primary_key_value().into();
            Some(crate::pagination::CursorPaginator::<M>::encode_value(
                &pk_val,
                crate::pagination::CursorDirection::Next,
            )?)
        } else {
            None
        };
        let prev_cursor = if flags.has_prev && !rows.is_empty() {
            let pk_val: sea_orm::Value = rows.first().unwrap().primary_key_value().into();
            Some(crate::pagination::CursorPaginator::<M>::encode_value(
                &pk_val,
                crate::pagination::CursorDirection::Prev,
            )?)
        } else {
            None
        };

        Ok(
            crate::pagination::CursorPaginator::new(rows, per_page, next_cursor, prev_cursor)
                .with_cursor_name("cursor"),
        )
    }

    // ---- Chunking + lazy iteration (Phase 10C T8) -----------------------

    /// Process rows in OFFSET-paginated batches of `n`.
    ///
    /// Each batch lands as a [`Collection<M>`] in the user's async
    /// closure. Memory is bounded by the batch size — never the full
    /// result set. The closure runs to completion before the next
    /// batch fetches, so a slow consumer doesn't accumulate rows.
    ///
    /// ## NOT safe under concurrent inserts
    ///
    /// OFFSET pagination skips rows that shift across the page
    /// boundary mid-iteration. If a row is inserted before the next
    /// batch's offset, it will be skipped; if a row is deleted before
    /// the next batch's offset, the row that took its place will be
    /// processed twice. Use [`Self::chunk_by_id`] for production-grade
    /// bulk processing — it filters on `id > last_id` and is
    /// concurrent-safe by construction.
    ///
    /// `chunk()` exists as the simple form for read-only workloads
    /// against stable tables and for models with non-`i64` primary
    /// keys where `chunk_by_id` cannot be used.
    ///
    /// ## Eager loads are not supported
    ///
    /// Pairing `.with(...)` with `.chunk(...)` returns
    /// `FrameworkError::internal` — the cross-batch builder clone
    /// drops the type-erased eager plan, so honouring the eager spec
    /// would be silently inconsistent. Re-apply `.with(...)` inside
    /// the per-chunk closure when needed (each batch's
    /// [`Collection<M>`] composes Laravel-shape with `load(...)` /
    /// `load_missing(...)` from T5b).
    ///
    /// ## Errors
    ///
    /// - `n == 0` → `FrameworkError::param("n")` (400). A zero batch
    ///   size would issue `LIMIT 0` forever; reject up front rather
    ///   than no-op silently.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// User::query().chunk(100, |batch| async move {
    ///     for user in &batch {
    ///         send_welcome_email(user).await?;
    ///     }
    ///     Ok(())
    /// }).await?;
    /// ```
    pub async fn chunk<F, Fut>(self, n: u64, mut f: F) -> Result<(), FrameworkError>
    where
        F: FnMut(Collection<M>) -> Fut + Send,
        Fut: std::future::Future<Output = Result<(), FrameworkError>> + Send,
    {
        if n == 0 {
            return Err(FrameworkError::param("n"));
        }
        if !self.eager_specs.is_empty() {
            return Err(FrameworkError::internal(
                "Builder::chunk does not support eager loading (`.with(...)`); apply `.with(...)` inside the per-chunk closure instead",
            ));
        }
        let mut offset: u64 = 0;
        loop {
            let q = self.clone().limit(n).offset(offset);
            let batch = q.get().await?;
            if batch.is_empty() {
                break;
            }
            let count = batch.len() as u64;
            f(batch).await?;
            if count < n {
                break;
            }
            offset = offset.saturating_add(n);
        }
        Ok(())
    }

    /// Process rows in PK-cursor batches of `n`. Concurrent-safe.
    ///
    /// Each batch issues `WHERE id > last_id ORDER BY id ASC LIMIT n`
    /// against the model's primary-key column, so rows inserted
    /// mid-iteration with PKs above the current cursor land in a
    /// later batch (rather than skipping or duplicating, which
    /// [`Self::chunk`]'s OFFSET form is vulnerable to).
    ///
    /// ## Requires an `i64` primary key
    ///
    /// The cursor is read off [`Model::field_value`] as an `i64` —
    /// models with `String` / `Uuid` PKs use [`Self::chunk`] with the
    /// OFFSET caveat (or wait for a follow-up that generalises the
    /// cursor shape). If [`Model::field_value`] returns a non-numeric
    /// JSON value for the PK column the loop breaks rather than
    /// looping forever; non-`i64` callers should reach for `chunk`.
    ///
    /// ## Eager loads
    ///
    /// Same restriction as [`Self::chunk`] — `.with(...)` is rejected
    /// up front. Re-apply inside the per-chunk closure as needed.
    ///
    /// ## Errors
    ///
    /// - `n == 0` → `FrameworkError::param("n")` (400). A zero batch
    ///   size would issue `LIMIT 0` forever; reject up front rather
    ///   than no-op silently.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // Process every user, surviving concurrent inserts.
    /// User::query().chunk_by_id(500, |batch| async move {
    ///     for user in &batch {
    ///         reindex_user(user).await?;
    ///     }
    ///     Ok(())
    /// }).await?;
    /// ```
    pub async fn chunk_by_id<F, Fut>(self, n: u64, mut f: F) -> Result<(), FrameworkError>
    where
        F: FnMut(Collection<M>) -> Fut + Send,
        Fut: std::future::Future<Output = Result<(), FrameworkError>> + Send,
    {
        if n == 0 {
            return Err(FrameworkError::param("n"));
        }
        if !self.eager_specs.is_empty() {
            return Err(FrameworkError::internal(
                "Builder::chunk_by_id does not support eager loading (`.with(...)`); apply `.with(...)` inside the per-chunk closure instead",
            ));
        }
        let pk = M::primary_key_name();
        let mut last_id: Option<i64> = None;
        loop {
            let mut q = self.clone().order_by_asc(pk).limit(n);
            if let Some(lid) = last_id {
                q = q.filter_op(pk, ">", lid);
            }
            let batch = q.get().await?;
            if batch.is_empty() {
                break;
            }
            // Read the highest PK in the batch (the rows came back
            // `ORDER BY pk ASC`, so `.last()` holds it). If the PK
            // can't be coerced to `i64` we bail rather than loop
            // forever — non-`i64` PK models should use `chunk()`.
            last_id = batch
                .last()
                .and_then(|m| m.field_value(pk))
                .and_then(|v| v.as_i64());
            let count = batch.len() as u64;
            f(batch).await?;
            if count < n {
                break;
            }
            if last_id.is_none() {
                return Err(FrameworkError::internal(
                    "Builder::chunk_by_id: primary key column did not yield an i64 value — \
                     models with non-i64 primary keys must use chunk() instead",
                ));
            }
        }
        Ok(())
    }

    /// OFFSET-paginated chunking with a per-chunk map. Returns the
    /// concatenated mapped results as a single [`Collection<U>`].
    ///
    /// Memory-bounded across the iteration: only one chunk's worth of
    /// `M` is in-flight at a time, while the accumulating
    /// `Collection<U>` retains every mapped item. Pick `U` smaller
    /// than `M` (a summary, an id, an aggregate) when the result is
    /// supposed to stay bounded across very large tables — otherwise
    /// switch to [`Self::chunk`] + an external sink.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// // Compute per-batch totals across the table; the result is
    /// // one i64 per batch, not per row.
    /// let totals: Collection<i64> = Order::query()
    ///     .chunk_map(1000, |batch| async move {
    ///         let sum: i64 = batch.iter().map(|o| o.amount).sum();
    ///         Ok(Collection::from_vec(vec![sum]))
    ///     })
    ///     .await?;
    /// ```
    pub async fn chunk_map<F, Fut, U>(
        self,
        n: u64,
        mut f: F,
    ) -> Result<Collection<U>, FrameworkError>
    where
        F: FnMut(Collection<M>) -> Fut + Send,
        Fut: std::future::Future<Output = Result<Collection<U>, FrameworkError>> + Send,
        U: Send,
    {
        // Same shape as `chunk`, but the per-iteration accumulator
        // lives in this scope so the mapped output can escape.
        // Delegating to `chunk` would force the accumulator into the
        // closure capture — the borrow checker rejects the resulting
        // `&mut out` aliasing across the async iteration.
        if !self.eager_specs.is_empty() {
            return Err(FrameworkError::internal(
                "Builder::chunk_map does not support eager loading (`.with(...)`); apply `.with(...)` inside the per-chunk closure instead",
            ));
        }
        let mut out: Vec<U> = Vec::new();
        let mut offset: u64 = 0;
        loop {
            let q = self.clone().limit(n).offset(offset);
            let batch = q.get().await?;
            if batch.is_empty() {
                break;
            }
            let count = batch.len() as u64;
            let mapped = f(batch).await?;
            out.extend(mapped.into_vec());
            if count < n {
                break;
            }
            offset = offset.saturating_add(n);
        }
        Ok(Collection::from_vec(out))
    }

    /// Process every row through `f` one at a time.
    ///
    /// Sugar for [`Self::chunk`]`(1, ...)` — issues N queries for N
    /// rows. For large datasets, switch to [`Self::lazy`] which
    /// internally batches via PK cursor (default 1000 rows per fetch)
    /// while still surfacing one row at a time.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// User::query().each(|user| async move {
    ///     send_welcome_email(&user).await?;
    ///     Ok(())
    /// }).await?;
    /// ```
    pub async fn each<F, Fut>(self, mut f: F) -> Result<(), FrameworkError>
    where
        F: FnMut(M) -> Fut + Send,
        Fut: std::future::Future<Output = Result<(), FrameworkError>> + Send,
    {
        // Inline OFFSET-paginated loop with batch size 1. Delegating
        // to `chunk(1, ...)` would force `f` into the closure
        // capture; the borrow checker rejects re-using the captured
        // `&mut f` across async iterations. Inline is the simplest
        // correct shape.
        if !self.eager_specs.is_empty() {
            return Err(FrameworkError::internal(
                "Builder::each does not support eager loading (`.with(...)`); apply `.with(...)` inside the per-row closure instead",
            ));
        }
        let mut offset: u64 = 0;
        loop {
            let q = self.clone().limit(1).offset(offset);
            let batch = q.get().await?;
            if batch.is_empty() {
                break;
            }
            let count = batch.len() as u64;
            for row in batch.into_vec() {
                f(row).await?;
            }
            if count < 1 {
                break;
            }
            offset = offset.saturating_add(1);
        }
        Ok(())
    }

    /// Stream rows one at a time via PK-cursor batching (default
    /// 1000 rows per fetch).
    ///
    /// The internal batch size keeps the round-trip count low; the
    /// returned [`LazyCollection<M>`](crate::eloquent::LazyCollection) surfaces one row at a time to
    /// the consumer with backpressure via `.next().await`.
    ///
    /// Alias: [`Self::cursor`] (Laravel name).
    ///
    /// ## Requires an `i64` primary key
    ///
    /// Same constraint as [`Self::chunk_by_id`] — the underlying
    /// batching uses an `id > last_id` filter. Models with `String` /
    /// `Uuid` PKs need [`Self::chunk`] until the cursor shape
    /// generalises.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let mut stream = User::query().lazy();
    /// while let Some(row) = stream.next().await {
    ///     let user = row?;
    ///     println!("{}", user.email);
    /// }
    /// ```
    pub fn lazy(self) -> crate::eloquent::LazyCollection<M> {
        self.lazy_by_id(1000)
    }

    /// Stream rows one at a time, with a custom internal batch size.
    ///
    /// Use this when the default 1000-row internal batch in
    /// [`Self::lazy`] isn't the right shape — e.g. very wide rows
    /// where 1000 in memory at once is too much, or very narrow rows
    /// where a larger batch reduces round trips.
    ///
    /// Same `i64`-PK constraint as [`Self::chunk_by_id`].
    pub fn lazy_by_id(self, batch_size: u64) -> crate::eloquent::LazyCollection<M> {
        let builder = self;
        let stream = async_stream::try_stream! {
            // Reject eager loads up front: they would be silently
            // dropped on the cross-batch clone, identical contract
            // to chunk()/chunk_by_id().
            if !builder.eager_specs.is_empty() {
                Err(FrameworkError::internal(
                    "Builder::lazy / lazy_by_id / cursor do not support eager loading (`.with(...)`); apply `.with(...)` inside the consumer loop instead",
                ))?;
            }
            let pk = M::primary_key_name();
            let mut last_id: Option<i64> = None;
            loop {
                let mut q = builder.clone().order_by_asc(pk).limit(batch_size);
                if let Some(lid) = last_id {
                    q = q.filter_op(pk, ">", lid);
                }
                let batch = q.get().await?;
                if batch.is_empty() {
                    break;
                }
                last_id = batch
                    .last()
                    .and_then(|m| m.field_value(pk))
                    .and_then(|v| v.as_i64());
                let count = batch.len() as u64;
                for row in batch.into_vec() {
                    yield row;
                }
                if count < batch_size {
                    break;
                }
                if last_id.is_none() {
                    Err(FrameworkError::internal(
                        "Builder::lazy_by_id: primary key column did not yield an i64 value — \
                         models with non-i64 primary keys cannot use lazy() / cursor()",
                    ))?;
                }
            }
        };
        crate::eloquent::LazyCollection::boxed(stream)
    }

    /// Laravel-shape alias for [`Self::lazy`].
    ///
    /// Same shape, same semantics, same `i64`-PK constraint. Ships
    /// alongside `lazy` so users with Laravel muscle memory don't
    /// have to translate.
    pub fn cursor(self) -> crate::eloquent::LazyCollection<M> {
        self.lazy()
    }

    // Terminal/aggregate type bounds are `TryGetable` — that's the
    // trait SeaORM's `QueryResult::try_get` uses to convert a column
    // value into a Rust type. `DeserializeOwned` (the serde bound) is
    // wrong here because the body reads from `try_get`, not from
    // `serde_json::from_value`. Common primitives (i64, f64, String,
    // bool, DateTime<Utc>) implement both, but a user passing a custom
    // type with only one of the two would hit a compile error against
    // a bound that doesn't match what the body actually uses.

    /// `SELECT COALESCE(SUM(col), 0)`. Returns `T::default()` on empty
    /// result sets.
    pub async fn sum<T: TryGetable + Default>(
        self,
        col: impl IntoColumn,
    ) -> Result<T, FrameworkError> {
        self.aggregate_value::<T>(&format!("COALESCE(SUM({}), 0)", col.col_name()))
            .await
    }

    /// `SELECT COALESCE(AVG(col), 0)`. Returns `T::default()` on empty
    /// result sets.
    pub async fn avg<T: TryGetable + Default>(
        self,
        col: impl IntoColumn,
    ) -> Result<T, FrameworkError> {
        self.aggregate_value::<T>(&format!("COALESCE(AVG({}), 0)", col.col_name()))
            .await
    }

    /// `SELECT MIN(col)`. Returns `None` on empty result sets.
    pub async fn min<T: TryGetable>(
        self,
        col: impl IntoColumn,
    ) -> Result<Option<T>, FrameworkError> {
        self.aggregate_optional::<T>(&format!("MIN({})", col.col_name()))
            .await
    }

    /// `SELECT MAX(col)`. Returns `None` on empty result sets.
    pub async fn max<T: TryGetable>(
        self,
        col: impl IntoColumn,
    ) -> Result<Option<T>, FrameworkError> {
        self.aggregate_optional::<T>(&format!("MAX({})", col.col_name()))
            .await
    }

    /// Fetch a single value from the first matching row.
    pub async fn value<T: TryGetable>(
        self,
        col: impl IntoColumn,
    ) -> Result<Option<T>, FrameworkError> {
        // T11/T12: respect `with_tx` + ambient CURRENT_TX + `on(name)`
        // + per-model default + `__read_replica__`.
        let exec = self.resolve_read_executor().await?;
        let backend = exec.backend();
        let mut s = self;
        s.limit = Some(1);
        let col_name = col.col_name();
        let (sql, vals) = s.render_select_for(backend, M::TABLE, &col_name)?;
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let row = exec
            .query_one(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(row.and_then(|r| r.try_get::<T>("", &col_name).ok()))
    }

    /// Fetch a single column from every matching row.
    pub async fn pluck<T: TryGetable>(
        self,
        col: impl IntoColumn,
    ) -> Result<Vec<T>, FrameworkError> {
        // T11/T12: respect `with_tx` + ambient CURRENT_TX + `on(name)`
        // + per-model default + `__read_replica__`.
        let exec = self.resolve_read_executor().await?;
        let backend = exec.backend();
        let col_name = col.col_name();
        let (sql, vals) = self.render_select_for(backend, M::TABLE, &col_name)?;
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let rows = exec
            .query_all(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.try_get::<T>("", &col_name).ok())
            .collect())
    }

    /// Fetch a `HashMap<K, V>` keyed by `key_col`, valued by `val_col`.
    pub async fn pluck_keyed<K: TryGetable + Eq + Hash, V: TryGetable>(
        self,
        key_col: impl IntoColumn,
        val_col: impl IntoColumn,
    ) -> Result<HashMap<K, V>, FrameworkError> {
        // T11/T12: respect `with_tx` + ambient CURRENT_TX + `on(name)`
        // + per-model default + `__read_replica__`.
        let exec = self.resolve_read_executor().await?;
        let backend = exec.backend();
        let kn = key_col.col_name();
        let vn = val_col.col_name();
        let (sql, vals) = self.render_select_for(backend, M::TABLE, &format!("{kn}, {vn}"))?;
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let rows = exec
            .query_all(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let mut out = HashMap::new();
        for r in rows {
            if let (Ok(k), Ok(v)) = (r.try_get::<K>("", &kn), r.try_get::<V>("", &vn)) {
                out.insert(k, v);
            }
        }
        Ok(out)
    }

    async fn aggregate_value<T: TryGetable + Default>(
        self,
        expr: &str,
    ) -> Result<T, FrameworkError> {
        // T11/T12: respect `with_tx` + ambient CURRENT_TX + `on(name)`
        // + per-model default + `__read_replica__`.
        let exec = self.resolve_read_executor().await?;
        let backend = exec.backend();
        let (sql, vals) = self.render_select_for(backend, M::TABLE, expr)?;
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let row = exec
            .query_one(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(row
            .and_then(|r| r.try_get::<T>("", expr).ok())
            .unwrap_or_default())
    }

    async fn aggregate_optional<T: TryGetable>(
        self,
        expr: &str,
    ) -> Result<Option<T>, FrameworkError> {
        // T11/T12: respect `with_tx` + ambient CURRENT_TX + `on(name)`
        // + per-model default + `__read_replica__`.
        let exec = self.resolve_read_executor().await?;
        let backend = exec.backend();
        let (sql, vals) = self.render_select_for(backend, M::TABLE, expr)?;
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let row = exec
            .query_one(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(row.and_then(|r| r.try_get::<T>("", expr).ok()))
    }

    // ---- Mass update / delete / upsert / increment_each / decrement_each --
    //
    // These methods compile to a single SQL statement that hits the
    // database directly — no per-row Model lifecycle hooks fire. Use
    // them when you have a WHERE-shape narrowing the scope and want to
    // mutate every matching row in one round-trip (the canonical
    // analogue of Laravel's `Model::query()->update([...])`).
    //
    // For per-row hooks (Saving / Updated / Deleted events), iterate
    // with `get()` and call `.save()` / `.update(...)` / `.delete()`
    // on each row instead.

    /// `UPDATE table SET <attrs> WHERE <where_terms>`. Returns the
    /// affected row count. Does NOT fire model events — for per-row
    /// hook dispatch iterate with `get()` and call `.update(attrs)`
    /// per row.
    ///
    /// Every column in `attrs` is interpolated as a SQL identifier
    /// (not a parameter — SQL doesn't allow that), and validated
    /// through [`crate::database::validate_identifier`]. Values are
    /// bound as parameters.
    pub async fn update_all(self, attrs: Attrs) -> Result<u64, FrameworkError> {
        if attrs.is_empty() {
            return Ok(0);
        }
        // Validate column names up front so an injection attempt fails
        // closed before the renderer interpolates them.
        for (k, _) in attrs.iter() {
            crate::database::validate_identifier(k)?;
        }
        self.validate_inputs()?;
        let exec = crate::database::transaction::ExecutorChoice::resolve_write(
            self.tx_override.as_ref(),
            self.connection_override.as_deref(),
            M::default_connection_name(),
        )
        .await?;
        let backend = exec.backend();

        let mut values: Vec<SeaValue> = Vec::new();
        let mut n: usize = 0;

        let mut sql = String::new();
        sql.push_str("UPDATE ");
        sql.push_str(M::TABLE);
        sql.push_str(" SET ");
        let set_parts: Vec<String> = attrs
            .iter()
            .map(|(col, v)| {
                n += 1;
                let ph = placeholder(backend, n);
                values.push(json_value_to_sea_value(v));
                format!("{col} = {ph}")
            })
            .collect();
        sql.push_str(&set_parts.join(", "));

        if !self.where_terms.is_empty() {
            sql.push_str(" WHERE ");
            let parts: Vec<String> = self
                .where_terms
                .iter()
                .map(|t| self.render_where_term(backend, t, &mut values, &mut n))
                .collect();
            sql.push_str(&parts.join(" AND "));
        }

        let stmt = Statement::from_sql_and_values(backend, &sql, values);
        let result = exec
            .run(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(result.rows_affected())
    }

    /// `DELETE FROM table WHERE <where_terms>`. Returns the affected
    /// row count. Mass-delete — no per-row Model events fire. For
    /// soft-delete model behaviour iterate with `get()` and call
    /// `.delete()` per row.
    pub async fn delete_all(self) -> Result<u64, FrameworkError> {
        self.validate_inputs()?;
        let exec = crate::database::transaction::ExecutorChoice::resolve_write(
            self.tx_override.as_ref(),
            self.connection_override.as_deref(),
            M::default_connection_name(),
        )
        .await?;
        let backend = exec.backend();
        let (sql, vals) = self.to_delete_sql_with_bindings_for(backend, M::TABLE);
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let result = exec
            .run(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(result.rows_affected())
    }

    /// Increment each column in `columns` by its mapped step. Atomic
    /// `UPDATE table SET col1 = col1 + ?, col2 = col2 + ? WHERE
    /// <where_terms>`. Returns the affected row count.
    ///
    /// Column names are validated through
    /// [`crate::database::validate_identifier`].
    pub async fn increment_each<I, S>(self, columns: I) -> Result<u64, FrameworkError>
    where
        I: IntoIterator<Item = (S, i64)>,
        S: Into<String>,
    {
        let owned: Vec<(String, i64)> = columns.into_iter().map(|(c, v)| (c.into(), v)).collect();
        for (c, _) in &owned {
            crate::database::validate_identifier(c)?;
        }
        if owned.is_empty() {
            return Ok(0);
        }
        self.validate_inputs()?;
        let exec = crate::database::transaction::ExecutorChoice::resolve_write(
            self.tx_override.as_ref(),
            self.connection_override.as_deref(),
            M::default_connection_name(),
        )
        .await?;
        let backend = exec.backend();
        let mut values: Vec<SeaValue> = Vec::new();
        let mut n: usize = 0;
        let mut sql = String::new();
        sql.push_str("UPDATE ");
        sql.push_str(M::TABLE);
        sql.push_str(" SET ");
        let set_parts: Vec<String> = owned
            .iter()
            .map(|(col, step)| {
                n += 1;
                let ph = placeholder(backend, n);
                values.push(SeaValue::BigInt(Some(*step)));
                format!("{col} = {col} + {ph}")
            })
            .collect();
        sql.push_str(&set_parts.join(", "));
        if !self.where_terms.is_empty() {
            sql.push_str(" WHERE ");
            let parts: Vec<String> = self
                .where_terms
                .iter()
                .map(|t| self.render_where_term(backend, t, &mut values, &mut n))
                .collect();
            sql.push_str(&parts.join(" AND "));
        }
        let stmt = Statement::from_sql_and_values(backend, &sql, values);
        let result = exec
            .run(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(result.rows_affected())
    }

    /// Decrement each column in `columns` by its mapped step. Sugar
    /// over [`Self::increment_each`] with the step negated.
    pub async fn decrement_each<I, S>(self, columns: I) -> Result<u64, FrameworkError>
    where
        I: IntoIterator<Item = (S, i64)>,
        S: Into<String>,
    {
        let negated: Vec<(String, i64)> =
            columns.into_iter().map(|(c, v)| (c.into(), -v)).collect();
        self.increment_each(negated).await
    }

    /// `INSERT ... ON CONFLICT (unique_by) DO UPDATE SET ...` (Postgres /
    /// SQLite) / `INSERT ... ON DUPLICATE KEY UPDATE ...` (MySQL).
    /// Mirrors Laravel's `Builder::upsert`.
    ///
    /// `rows` carries the values to insert; `unique_by` carries the
    /// columns that identify a "duplicate" (the conflict target); when
    /// `update` is `Some`, only those columns receive the conflict-side
    /// `SET` clause (defaults to every column in the first row's keyset
    /// except the unique-by columns when `None`).
    ///
    /// Returns the affected row count. Does NOT fire per-row model
    /// events — use [`Model::create`] / [`Model::update`] from a loop
    /// for that.
    pub async fn upsert(
        self,
        rows: Vec<Attrs>,
        unique_by: Vec<&str>,
        update: Option<Vec<&str>>,
    ) -> Result<u64, FrameworkError> {
        if rows.is_empty() {
            return Ok(0);
        }
        for c in &unique_by {
            crate::database::validate_identifier(c)?;
        }
        if let Some(cs) = &update {
            for c in cs {
                crate::database::validate_identifier(c)?;
            }
        }
        // Determine the column order from the first row; all rows must
        // share the same column set.
        let cols: Vec<String> = rows[0].iter().map(|(k, _)| k.to_string()).collect();
        for c in &cols {
            crate::database::validate_identifier(c)?;
        }
        let update_cols: Vec<String> = match update {
            Some(cs) => cs.iter().map(|s| s.to_string()).collect(),
            None => cols
                .iter()
                .filter(|c| !unique_by.contains(&c.as_str()))
                .cloned()
                .collect(),
        };

        let exec = crate::database::transaction::ExecutorChoice::resolve_write(
            self.tx_override.as_ref(),
            self.connection_override.as_deref(),
            M::default_connection_name(),
        )
        .await?;
        let backend = exec.backend();

        let mut values: Vec<SeaValue> = Vec::new();
        let mut n: usize = 0;

        let mut sql = String::new();
        sql.push_str("INSERT INTO ");
        sql.push_str(M::TABLE);
        sql.push_str(" (");
        sql.push_str(&cols.join(", "));
        sql.push_str(") VALUES ");
        let row_parts: Vec<String> = rows
            .iter()
            .map(|attrs| {
                let phs: Vec<String> = cols
                    .iter()
                    .map(|c| {
                        let v = attrs.get(c).cloned().unwrap_or(Value::Null);
                        n += 1;
                        let ph = placeholder(backend, n);
                        values.push(json_value_to_sea_value(&v));
                        ph
                    })
                    .collect();
                format!("({})", phs.join(", "))
            })
            .collect();
        sql.push_str(&row_parts.join(", "));

        match backend {
            DbBackend::Postgres | DbBackend::Sqlite => {
                sql.push_str(" ON CONFLICT (");
                sql.push_str(&unique_by.join(", "));
                sql.push_str(") DO UPDATE SET ");
                let set_parts: Vec<String> = update_cols
                    .iter()
                    .map(|c| format!("{c} = EXCLUDED.{c}"))
                    .collect();
                sql.push_str(&set_parts.join(", "));
            }
            DbBackend::MySql => {
                sql.push_str(" ON DUPLICATE KEY UPDATE ");
                let set_parts: Vec<String> = update_cols
                    .iter()
                    .map(|c| format!("{c} = VALUES({c})"))
                    .collect();
                sql.push_str(&set_parts.join(", "));
            }
        }

        let stmt = Statement::from_sql_and_values(backend, &sql, values);
        let result = exec
            .run(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(result.rows_affected())
    }
}

// ---- Pagination request helpers -----------------------------------------

/// Read the current page number from the request's query string via
/// the [`Context`][crate::context::Context] facade. Defaults to `1`
/// when the parameter is missing, empty, non-numeric, or zero.
///
/// Used by [`Builder::paginate`] / [`Builder::paginate_using`] /
/// [`Builder::simple_paginate`] to derive the offset for the page
/// query.
fn current_page_from_request(param: &str) -> u64 {
    crate::context::Context::query_param(param)
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(1)
}

/// Read the opaque cursor from the request's `?cursor=...` query
/// parameter. Returns `None` when the parameter is missing or empty.
fn current_cursor_from_request() -> Option<String> {
    crate::context::Context::query_param("cursor").filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn into_column_for_str() {
        assert_eq!("email".col_name(), "email");
    }

    #[test]
    fn into_column_for_string() {
        let s = String::from("name");
        assert_eq!(s.col_name(), "name");
    }

    #[test]
    fn into_column_for_string_ref() {
        let s = String::from("created_at");
        assert_eq!((&s).col_name(), "created_at");
    }

    #[test]
    fn direction_sql() {
        assert_eq!(Direction::Asc.sql(), "ASC");
        assert_eq!(Direction::Desc.sql(), "DESC");
    }
}
