//! Builder<M> — the chainable query type returned by `Model::query()`.
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
//! ## Per-WhereTerm SQL renderer
//!
//! [`Builder::render_select_for`] emits per-backend SQL from the
//! [`WhereTerm`] AST: Postgres `$N` placeholders + `EXTRACT(... FROM
//! col)` date parts + `@>` JSON containment; MySQL + SQLite use `?`
//! placeholders with backend-appropriate `DATE()` / `JSON_LENGTH()`
//! forms.
//!
//! UNION arms thread the placeholder counter through
//! [`Builder::render_select_into`] so Postgres `$N` numbering stays
//! monotonic across the combined statement — see
//! `union_postgres_placeholders_are_monotonic` in
//! `framework/tests/eloquent_builder.rs` for the regression test.

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
use crate::eloquent::attrs::Attrs;
use crate::eloquent::model::{json_value_to_sea_value, Model};
use crate::eloquent::EloquentModel;
use crate::error::FrameworkError;

// ---- IntoColumn / IntoVal ------------------------------------------------

/// Convert a value into a column name for use with `Builder<M>` methods.
/// Implemented by every macro-generated `Column` enum so users can write
/// either typed (`Column::Email`) or string (`"email"`) arguments
/// throughout the builder API.
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
    Asc,
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
    /// Eager-load spec list — populated by [`Builder::with`].
    ///
    /// Each entry is a relation name (e.g. `"profile"`). At
    /// [`Builder::get`] time, every entry triggers a call into the
    /// model's `__eager_load` dispatcher (via [`EagerLoadDispatch`])
    /// after the base SELECT lands. T2 ships the flat-list form;
    /// T9 owns the full nested-path / `with_count` / `with_sum`-`max`
    /// surface.
    pub(crate) eager_specs: Vec<String>,
    _phantom: PhantomData<M>,
}

impl<M> Default for Builder<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> Builder<M> {
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
            eager_specs: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Append relation names to the eager-load spec list. Called by
    /// the macro-emitted `Self::with(...)` shortcut; user code can
    /// also chain directly off a `Builder<Self>`.
    ///
    /// T2 ships the flat-list form. T9 will extend this with nested
    /// paths (`"posts.comments"`), `with_count` / `with_sum` /
    /// `with_avg` / `with_min` / `with_max`, and `with_where`
    /// predicates. The fancy variants will live in T9-specific
    /// methods on `Builder<M>` — `with(...)` keeps the simple flat
    /// list shape.
    pub fn with<I, S>(mut self, relations: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for r in relations {
            self.eager_specs.push(r.into());
        }
        self
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
        self.where_terms
            .push(WhereTerm::Op(col.col_name(), op.to_string(), val.into_val()));
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
    pub fn order_by_raw(mut self, sql: impl Into<String>) -> Self {
        self.orders.push(OrderTerm::Raw(sql.into()));
        self
    }

    /// `ORDER BY RANDOM()` — useful for sampling. Each backend emits
    /// its own randomisation function via [`render_orders`].
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
        self.having_terms
            .push(WhereTerm::Op(col.col_name(), op.to_string(), val.into_val()));
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
        self.select_cols.get_or_insert_with(Vec::new).push(col.into());
        self
    }

    /// Replace the SELECT column list with a raw SQL fragment
    /// (`COUNT(*) AS total`, `name, COUNT(role) OVER (...)`, ...).
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

    /// Override the model's static cast pipeline for this query. T7b
    /// consumes this; T5 just plumbs it through.
    pub fn with_casts(
        mut self,
        casts: HashMap<&'static str, std::sync::Arc<dyn crate::eloquent::casts::DynCast>>,
    ) -> Self {
        self.runtime_casts = casts;
        self
    }

    /// Skip a named global scope on this query. T10 consumes this; T5
    /// just plumbs it through.
    pub fn without_global_scope(mut self, name: &'static str) -> Self {
        self.global_scopes_disabled.push(name);
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
    ) -> (String, Vec<SeaValue>) {
        let mut values: Vec<SeaValue> = Vec::new();
        let mut n = 0;
        let sql = self.render_select_into(backend, table, column_expr, &mut values, &mut n);
        (sql, values)
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
    /// Render the SQL for debugging. Uses the live DB connection's
    /// backend if one is initialised, otherwise falls back to SQLite
    /// shape so tests without a connection still get deterministic
    /// output. For explicit-dialect rendering use [`Self::to_sql_for`].
    pub fn to_sql(&self) -> String {
        self.to_sql_with_bindings().0
    }

    /// Render the SQL for the live DB connection's backend, returning
    /// both the SQL string and the bound values.
    pub fn to_sql_with_bindings(&self) -> (String, Vec<SeaValue>) {
        let backend = DB::connection()
            .ok()
            .map(|db| db.inner().get_database_backend())
            .unwrap_or(DbBackend::Sqlite);
        self.render_select_for(backend, M::TABLE, "*")
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
    pub async fn get(self) -> Result<Vec<M>, FrameworkError> {
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let runtime_casts = self.runtime_casts.clone();
        let eager_specs = self.eager_specs.clone();
        let (sql, vals) = self.render_select_for(backend, M::TABLE, "*");
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);

        // Fetch into the entity's `Model` — the SeaORM type that's
        // auto-implementing `FromQueryResult`. This is the storage-shape
        // type, not the user's runtime struct.
        let raw_rows = <<M as EloquentModel>::Entity as sea_orm::EntityTrait>::Model
            ::find_by_statement(stmt)
            .all(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        let mut out: Vec<M> = if runtime_casts.is_empty() {
            // Fast path — convert each row directly via the
            // macro-emitted `From<inner::Model> for M`.
            raw_rows.into_iter().map(M::from).collect()
        } else {
            // Slow path (override mode) — serialise the storage-shape
            // row to JSON, apply each runtime cast in place, then
            // deserialise into M. Static casts on M are NOT applied;
            // the runtime cast map is treated as a full replacement
            // for this query.
            let mut buf = Vec::with_capacity(raw_rows.len());
            for row in raw_rows {
                let mut as_json = serde_json::to_value(&row).map_err(|e| {
                    FrameworkError::database(format!(
                        "serialise inner Model for runtime cast: {e}"
                    ))
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

        // T2 — eager loading. After the base SELECT lands, walk the
        // recorded eager specs and dispatch into the per-model
        // `__eager_load` (via the `EagerLoadDispatch` trait the macro
        // emits). Each call mutates every row's `__eager` cache
        // in-place. T9 will extend this with nested-path / `with_count`
        // / `with_sum`-`max` / `with_where`.
        if !eager_specs.is_empty() && !out.is_empty() {
            for spec in &eager_specs {
                // SAFETY-equivalent borrow plumbing: the dispatcher
                // signature is `&mut [&mut Self]`, so we build a
                // scratch vec of `&mut M` borrows from `out` for the
                // duration of this call. Borrow checker hates a
                // straight `iter_mut().collect::<Vec<&mut M>>()`
                // bound to `'a`, so we scope each call so the borrow
                // lifetime ends before the next iteration starts.
                let mut refs: Vec<&mut M> = out.iter_mut().collect();
                <M as crate::eloquent::EagerLoadDispatch>::eager_load(
                    spec.as_str(),
                    refs.as_mut_slice(),
                    db.inner(),
                    None,
                )
                .await?;
            }
        }

        Ok(out)
    }

    /// Execute the SELECT and return at most one row.
    pub async fn first(mut self) -> Result<Option<M>, FrameworkError> {
        self.limit = Some(1);
        let mut rows = self.get().await?;
        Ok(rows.pop())
    }

    /// Execute the SELECT and return one row. Errors with
    /// `FrameworkError::ModelNotFound` (HTTP 404) if no row matches.
    pub async fn first_or_fail(self) -> Result<M, FrameworkError> {
        self.first()
            .await?
            .ok_or_else(|| FrameworkError::not_found("no rows matched"))
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
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let mut s = self;
        s.limit = Some(1);
        let col_name = col.col_name();
        let (sql, vals) = s.render_select_for(backend, M::TABLE, &col_name);
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let row = db
            .inner()
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
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let col_name = col.col_name();
        let (sql, vals) = self.render_select_for(backend, M::TABLE, &col_name);
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let rows = db
            .inner()
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
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let kn = key_col.col_name();
        let vn = val_col.col_name();
        let (sql, vals) = self.render_select_for(backend, M::TABLE, &format!("{kn}, {vn}"));
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let rows = db
            .inner()
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
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let (sql, vals) = self.render_select_for(backend, M::TABLE, expr);
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let row = db
            .inner()
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
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let (sql, vals) = self.render_select_for(backend, M::TABLE, expr);
        let stmt = Statement::from_sql_and_values(backend, &sql, vals);
        let row = db
            .inner()
            .query_one(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(row.and_then(|r| r.try_get::<T>("", expr).ok()))
    }
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
