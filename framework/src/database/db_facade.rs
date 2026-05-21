//! Phase 10C T10 — `DB` facade extensions for model-less queries.
//!
//! This file ships two surfaces:
//!
//! 1. [`DbTableBuilder`] — a chainable query builder returned by
//!    [`DB::table(name)`](crate::DB::table). Mirrors the
//!    `filter`/`order_by`/`limit` shape of `Builder<M>` but materialises
//!    rows as [`DynamicRow`] instead of a typed model. Use it for
//!    tables that aren't worth a full `#[suprnova::model]` (audit
//!    logs, ad-hoc reports, dashboard aggregates).
//!
//! 2. Raw-SQL escapes — [`DB::select`](crate::DB::select),
//!    [`DB::update`](crate::DB::update), [`DB::delete`](crate::DB::delete),
//!    [`DB::statement`](crate::DB::statement),
//!    [`DB::affecting_statement`](crate::DB::affecting_statement). When
//!    the builder isn't enough (window functions, recursive CTEs,
//!    backend-specific DDL), drop to a raw string with placeholder
//!    bindings.
//!
//! ## Trust boundary on identifiers
//!
//! Table names, column names, SQL operators, and ORDER BY directions
//! are interpolated INTO the SQL string verbatim — they are NOT bound
//! as parameters (SQL doesn't allow that). Treat every `impl
//! Into<String>` argument to this builder as a trusted, compile-time
//! literal: do NOT splice user input into table or column names.
//! Values (the right-hand side of `filter` / `filter_op`) ARE bound
//! as parameters and safe to pass through from request data.
//!
//! Backend-aware placeholder generation: `$N` (Postgres) vs `?`
//! (MySQL + SQLite). The counter is monotonic across the SET clause
//! and WHERE clause in UPDATE statements so each binding lines up with
//! its corresponding position.

use crate::database::dynamic_row::DynamicRow;
use crate::database::DB;
use crate::eloquent::attrs::Attrs;
use crate::eloquent::builder::Direction;
use crate::eloquent::Collection;
use crate::FrameworkError;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, JsonValue, Statement, Value as SeaValue};

/// Standalone query builder returned by
/// [`DB::table(name)`](crate::DB::table). Mirrors the where / order /
/// limit / select shape of [`Builder<M>`](crate::eloquent::Builder)
/// but materialises rows as [`DynamicRow`] instead of a typed model.
///
/// See the [module docs](self) for the trust boundary on identifiers.
pub struct DbTableBuilder {
    table: String,
    where_terms: Vec<(String, String, SeaValue)>,
    order: Vec<(String, Direction)>,
    limit_value: Option<u64>,
    offset_value: Option<u64>,
    select_columns: Vec<String>,
}

impl DbTableBuilder {
    /// Construct a builder for the given table. Prefer
    /// [`DB::table(name)`](crate::DB::table) — this is the underlying
    /// constructor.
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            where_terms: Vec::new(),
            order: Vec::new(),
            limit_value: None,
            offset_value: None,
            select_columns: Vec::new(),
        }
    }

    /// Restrict the SELECT to a specific column list. Empty means `*`.
    ///
    /// ```rust,ignore
    /// DB::table("audit_log").select(["id", "event"]).get().await?;
    /// ```
    pub fn select<I, S>(mut self, cols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.select_columns = cols.into_iter().map(|s| s.into()).collect();
        self
    }

    /// Add a `WHERE col = ?` clause. Multiple `filter` calls AND together.
    pub fn filter(mut self, col: impl Into<String>, val: impl Into<SeaValue>) -> Self {
        self.where_terms.push((col.into(), "=".into(), val.into()));
        self
    }

    /// Add a `WHERE col <op> ?` clause with an explicit operator
    /// (`>`, `>=`, `<`, `<=`, `<>`, `LIKE`, etc.). Multiple calls AND
    /// together.
    pub fn filter_op(
        mut self,
        col: impl Into<String>,
        op: impl Into<String>,
        val: impl Into<SeaValue>,
    ) -> Self {
        self.where_terms.push((col.into(), op.into(), val.into()));
        self
    }

    /// Add an `ORDER BY col DESC` term. Multiple `order_by_*` calls
    /// chain in insertion order.
    pub fn order_by_desc(mut self, col: impl Into<String>) -> Self {
        self.order.push((col.into(), Direction::Desc));
        self
    }

    /// Add an `ORDER BY col ASC` term.
    pub fn order_by_asc(mut self, col: impl Into<String>) -> Self {
        self.order.push((col.into(), Direction::Asc));
        self
    }

    /// Set the LIMIT.
    pub fn limit(mut self, n: u64) -> Self {
        self.limit_value = Some(n);
        self
    }

    /// Set the OFFSET.
    pub fn offset(mut self, n: u64) -> Self {
        self.offset_value = Some(n);
        self
    }

    /// Execute the SELECT and return every matching row as a
    /// [`Collection<DynamicRow>`]. Uses
    /// [`sea_orm::JsonValue::find_by_statement`] under the hood so
    /// column shape is discovered at runtime.
    pub async fn get(self) -> Result<Collection<DynamicRow>, FrameworkError> {
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let (sql, values) = self.render_select(backend);
        let stmt = Statement::from_sql_and_values(backend, &sql, values);

        let rows = JsonValue::find_by_statement(stmt)
            .all(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        let dyn_rows: Vec<DynamicRow> = rows
            .into_iter()
            .filter_map(|v| match v {
                serde_json::Value::Object(map) => Some(DynamicRow::from_map(map)),
                _ => None,
            })
            .collect();

        Ok(Collection::from_vec(dyn_rows))
    }

    /// Execute the SELECT with `LIMIT 1` and return the single row
    /// (or `None` when the result set is empty).
    pub async fn first(self) -> Result<Option<DynamicRow>, FrameworkError> {
        let rows = self.limit(1).get().await?.into_vec();
        Ok(rows.into_iter().next())
    }

    /// Execute `SELECT COUNT(*) FROM ... WHERE ...` and return the
    /// count. Clears `select_columns` / `order` / `limit` / `offset`
    /// before rendering — count semantics don't care about those.
    ///
    /// Uses `query_one` + `try_get` directly instead of
    /// `JsonValue::find_by_statement` because aggregate columns
    /// (`COUNT(*)`) don't always carry a type tag through sqlx's
    /// per-column type detection that backs `JsonValue`'s
    /// `FromQueryResult` impl — on SQLite the typed accessor is the
    /// reliable path.
    pub async fn count(self) -> Result<u64, FrameworkError> {
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let mut copy = self;
        copy.select_columns = vec!["COUNT(*) as count".into()];
        copy.order.clear();
        copy.limit_value = None;
        copy.offset_value = None;
        let (sql, values) = copy.render_select(backend);
        let stmt = Statement::from_sql_and_values(backend, &sql, values);

        let row = db
            .inner()
            .query_one(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        let count: i64 = row
            .as_ref()
            .and_then(|r| r.try_get::<i64>("", "count").ok())
            .unwrap_or(0);
        Ok(count.max(0) as u64)
    }

    /// Insert one row.
    ///
    /// Returns the inserted row's primary key (assumes `id`). Backend
    /// split: Postgres + SQLite use `RETURNING id`; MySQL runs the
    /// INSERT then issues `SELECT LAST_INSERT_ID()`.
    pub async fn insert(self, attrs: Attrs) -> Result<i64, FrameworkError> {
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();

        let cols: Vec<String> = attrs.keys().map(String::from).collect();
        if cols.is_empty() {
            return Err(FrameworkError::database(format!(
                "DB::table(\"{}\")::insert called with empty attrs",
                self.table
            )));
        }

        let placeholders: Vec<String> = (0..cols.len())
            .map(|i| {
                if backend == DbBackend::Postgres {
                    format!("${}", i + 1)
                } else {
                    "?".into()
                }
            })
            .collect();

        let values: Vec<SeaValue> = cols
            .iter()
            .map(|c| {
                let v = attrs.get(c).expect("key present in iter must be present in get");
                crate::eloquent::model::json_value_to_sea_value(v)
            })
            .collect();

        let base = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            self.table,
            cols.join(", "),
            placeholders.join(", "),
        );

        match backend {
            DbBackend::Postgres | DbBackend::Sqlite => {
                let sql = format!("{base} RETURNING id");
                let stmt = Statement::from_sql_and_values(backend, &sql, values);
                let row = db
                    .inner()
                    .query_one(stmt)
                    .await
                    .map_err(|e| FrameworkError::database(e.to_string()))?;
                let id = row
                    .and_then(|r| r.try_get::<i64>("", "id").ok())
                    .unwrap_or(0);
                Ok(id)
            }
            DbBackend::MySql => {
                // MySQL doesn't support `RETURNING`. Use the driver's
                // own per-connection `last_insert_id()` exposed through
                // `ExecResult` — running a separate `SELECT
                // LAST_INSERT_ID()` against a pooled connection would
                // be unsafe because the SELECT might land on a
                // different physical connection than the INSERT.
                let stmt = Statement::from_sql_and_values(backend, &base, values);
                let result = db
                    .inner()
                    .execute(stmt)
                    .await
                    .map_err(|e| FrameworkError::database(e.to_string()))?;
                Ok(result.last_insert_id() as i64)
            }
        }
    }

    /// Update every row matched by the WHERE clauses. Returns the
    /// number of rows affected.
    ///
    /// **Empty WHERE updates every row in the table.** That's a
    /// supported but rarely-correct operation — callers should add at
    /// least one `filter` unless they really mean "all rows."
    pub async fn update(self, attrs: Attrs) -> Result<u64, FrameworkError> {
        if attrs.is_empty() {
            return Err(FrameworkError::database(format!(
                "DB::table(\"{}\")::update called with empty attrs",
                self.table
            )));
        }
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let (sql, values) = self.render_update(&attrs, backend);
        let stmt = Statement::from_sql_and_values(backend, &sql, values);
        let result = db
            .inner()
            .execute(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(result.rows_affected())
    }

    /// Delete every row matched by the WHERE clauses. Returns the
    /// number of rows affected.
    ///
    /// **Empty WHERE truncates the table.** `DB::table("x").delete()`
    /// removes every row by design — add a `filter` if you don't mean
    /// that.
    pub async fn delete(self) -> Result<u64, FrameworkError> {
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let (sql, values) = self.render_delete(backend);
        let stmt = Statement::from_sql_and_values(backend, &sql, values);
        let result = db
            .inner()
            .execute(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(result.rows_affected())
    }

    // ---- SQL rendering ---------------------------------------------------

    fn render_select(&self, backend: DbBackend) -> (String, Vec<SeaValue>) {
        let mut values: Vec<SeaValue> = Vec::new();
        let mut sql = String::new();
        sql.push_str("SELECT ");
        if self.select_columns.is_empty() {
            sql.push('*');
        } else {
            sql.push_str(&self.select_columns.join(", "));
        }
        sql.push_str(" FROM ");
        sql.push_str(&self.table);

        if !self.where_terms.is_empty() {
            sql.push_str(" WHERE ");
            let mut counter = 0usize;
            let clauses: Vec<String> = self
                .where_terms
                .iter()
                .map(|(col, op, val)| {
                    counter += 1;
                    values.push(val.clone());
                    if backend == DbBackend::Postgres {
                        format!("{col} {op} ${counter}")
                    } else {
                        format!("{col} {op} ?")
                    }
                })
                .collect();
            sql.push_str(&clauses.join(" AND "));
        }

        if !self.order.is_empty() {
            sql.push_str(" ORDER BY ");
            let order: Vec<String> = self
                .order
                .iter()
                .map(|(col, dir)| {
                    let dir_sql = match dir {
                        Direction::Asc => "ASC",
                        Direction::Desc => "DESC",
                    };
                    format!("{col} {dir_sql}")
                })
                .collect();
            sql.push_str(&order.join(", "));
        }

        if let Some(n) = self.limit_value {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        if let Some(n) = self.offset_value {
            sql.push_str(&format!(" OFFSET {n}"));
        }

        (sql, values)
    }

    fn render_update(&self, attrs: &Attrs, backend: DbBackend) -> (String, Vec<SeaValue>) {
        let mut values: Vec<SeaValue> = Vec::new();
        let mut counter = 0usize;

        let mut sql = format!("UPDATE {} SET ", self.table);
        let sets: Vec<String> = attrs
            .keys()
            .map(|col| {
                counter += 1;
                let v = attrs
                    .get(col)
                    .expect("key present in iter must be present in get");
                values.push(crate::eloquent::model::json_value_to_sea_value(v));
                if backend == DbBackend::Postgres {
                    format!("{col} = ${counter}")
                } else {
                    format!("{col} = ?")
                }
            })
            .collect();
        sql.push_str(&sets.join(", "));

        if !self.where_terms.is_empty() {
            sql.push_str(" WHERE ");
            let clauses: Vec<String> = self
                .where_terms
                .iter()
                .map(|(col, op, val)| {
                    counter += 1;
                    values.push(val.clone());
                    if backend == DbBackend::Postgres {
                        format!("{col} {op} ${counter}")
                    } else {
                        format!("{col} {op} ?")
                    }
                })
                .collect();
            sql.push_str(&clauses.join(" AND "));
        }

        (sql, values)
    }

    fn render_delete(&self, backend: DbBackend) -> (String, Vec<SeaValue>) {
        let mut values: Vec<SeaValue> = Vec::new();
        let mut sql = format!("DELETE FROM {}", self.table);
        if !self.where_terms.is_empty() {
            sql.push_str(" WHERE ");
            let mut counter = 0usize;
            let clauses: Vec<String> = self
                .where_terms
                .iter()
                .map(|(col, op, val)| {
                    counter += 1;
                    values.push(val.clone());
                    if backend == DbBackend::Postgres {
                        format!("{col} {op} ${counter}")
                    } else {
                        format!("{col} {op} ?")
                    }
                })
                .collect();
            sql.push_str(&clauses.join(" AND "));
        }
        (sql, values)
    }
}

// ---- DB facade extensions -----------------------------------------------

impl DB {
    /// Open a model-less query builder for `name`. See
    /// [`DbTableBuilder`] for the chainable surface.
    ///
    /// ```rust,ignore
    /// let rows = DB::table("audit_log")
    ///     .filter("actor_id", 42)
    ///     .order_by_desc("id")
    ///     .limit(50)
    ///     .get()
    ///     .await?;
    /// ```
    pub fn table(name: impl Into<String>) -> DbTableBuilder {
        DbTableBuilder::new(name)
    }

    /// Run a raw SELECT and return every row as a [`DynamicRow`].
    /// Placeholders must match the active backend (`$1, $2, ...` for
    /// Postgres, `?` for MySQL + SQLite).
    ///
    /// ```rust,ignore
    /// let rows = DB::select(
    ///     "SELECT * FROM audit_log WHERE actor_id = ?",
    ///     vec![42i64.into()],
    /// ).await?;
    /// ```
    pub async fn select(
        sql: &str,
        values: impl IntoIterator<Item = SeaValue>,
    ) -> Result<Vec<DynamicRow>, FrameworkError> {
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let stmt =
            Statement::from_sql_and_values(backend, sql, values.into_iter().collect::<Vec<_>>());
        let rows = JsonValue::find_by_statement(stmt)
            .all(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(rows
            .into_iter()
            .filter_map(|v| match v {
                serde_json::Value::Object(map) => Some(DynamicRow::from_map(map)),
                _ => None,
            })
            .collect())
    }

    /// Run a raw UPDATE and return the number of rows affected.
    /// Convenience alias over [`DB::affecting_statement`].
    pub async fn update(
        sql: &str,
        values: impl IntoIterator<Item = SeaValue>,
    ) -> Result<u64, FrameworkError> {
        DB::affecting_statement(sql, values).await
    }

    /// Run a raw DELETE and return the number of rows affected.
    /// Convenience alias over [`DB::affecting_statement`].
    pub async fn delete(
        sql: &str,
        values: impl IntoIterator<Item = SeaValue>,
    ) -> Result<u64, FrameworkError> {
        DB::affecting_statement(sql, values).await
    }

    /// Run a DDL statement (or any statement that takes no bindings).
    /// Discards the result — use this for `CREATE INDEX`, `ALTER
    /// TABLE`, `VACUUM`, etc.
    pub async fn statement(sql: &str) -> Result<(), FrameworkError> {
        let db = DB::connection()?;
        db.inner()
            .execute_unprepared(sql)
            .await
            .map(|_| ())
            .map_err(|e| FrameworkError::database(e.to_string()))
    }

    /// Run a raw statement that produces a `rows_affected` result.
    /// Used by [`DB::update`] and [`DB::delete`] under the hood;
    /// exposed directly for cases where the operation doesn't fit
    /// either name (e.g. `INSERT ... ON CONFLICT DO UPDATE`).
    pub async fn affecting_statement(
        sql: &str,
        values: impl IntoIterator<Item = SeaValue>,
    ) -> Result<u64, FrameworkError> {
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let stmt =
            Statement::from_sql_and_values(backend, sql, values.into_iter().collect::<Vec<_>>());
        let result = db
            .inner()
            .execute(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(result.rows_affected())
    }
}
