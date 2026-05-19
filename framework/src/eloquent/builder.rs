//! Eloquent query builder primitives.
//!
//! Phase 10A T3 ships the [`IntoColumn`] trait — the bridge that lets
//! later `Builder<M>` methods (`filter`, `db_where`, `order_by`, ...)
//! accept either typed `Column` variants or string column names. The
//! macro-emitted `Column` enums impl `IntoColumn` directly; `&str` and
//! `String` impl it via the runtime path.
//!
//! Phase 10A T4 adds the [`Builder<M>`] type and a single terminal
//! ([`Builder::first`]) plus the `filter_attrs` constructor —
//! collectively just enough surface for `first_or_create`,
//! `update_or_create`, and `first_or_new` to short-circuit when a
//! matching row exists.
//!
//! Phase 10A T5 replaces this stub with the full dual-API builder
//! pipeline (`filter` / `db_where`, ordering, grouping, aggregates,
//! terminals).
//!
//! Surface visibility is `pub`: `suprnova::eloquent::builder::IntoColumn`
//! is the documented re-export path. The macro emits qualified paths
//! against that location.
//!
//! Type-bridge invariant: `Column::from_name("not_a_column")` returns
//! `None`. The string-based `IntoColumn` impls return the input string
//! verbatim; the eventual SQL builder is responsible for catching
//! unknown columns and producing a user-friendly error.

use std::marker::PhantomData;

use sea_orm::{ConnectionTrait, FromQueryResult, Statement, Value as SeaValue};
use serde_json::Value;

use crate::database::DB;
use crate::eloquent::attrs::Attrs;
use crate::eloquent::EloquentModel;
use crate::error::FrameworkError;

/// Convert a value into a column name for use with Eloquent's
/// `Builder<M>` methods. Implemented by every macro-generated `Column`
/// enum so users can write either typed (`Column::Email`) or string
/// (`"email"`) arguments throughout the builder API.
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

/// Trait describing a model that can be loaded from SQL by its
/// `Entity::Model` row. Bound through `EloquentModel` so the macro
/// emission stays the single source of truth.
pub trait FromRow: EloquentModel + From<<Self::Entity as sea_orm::EntityTrait>::Model> {}

impl<T> FromRow for T where
    T: EloquentModel + From<<T::Entity as sea_orm::EntityTrait>::Model>
{
}

/// Builder over `M`. T4 ships a minimal SQL-only stub used by
/// `first_or_create` and friends to look up a row by attribute
/// equality. T5 replaces the implementation entirely.
pub struct Builder<M> {
    pub(crate) attrs_filters: Vec<(String, Value)>,
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
            attrs_filters: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Append the `(column, value)` pairs from an `Attrs` map onto the
    /// builder's WHERE clauses. T4 uses this from `first_or_create` /
    /// `update_or_create` / `first_or_new` to look a row up by exact
    /// attribute equality. The public dual-API `filter` / `db_where`
    /// methods land in T5.
    pub(crate) fn filter_attrs(mut self, attrs: &Attrs) -> Self {
        for (k, v) in attrs.iter() {
            self.attrs_filters.push((k.to_string(), v.clone()));
        }
        self
    }

    /// Execute the query and return at most one row.
    pub async fn first(self) -> Result<Option<M>, FrameworkError>
    where
        M: FromRow,
    {
        let db = DB::connection()?;
        let backend = db.inner().get_database_backend();
        let table = M::TABLE;
        let mut sql = format!("SELECT * FROM {table}");
        let mut values: Vec<SeaValue> = Vec::new();
        if !self.attrs_filters.is_empty() {
            let clauses: Vec<String> = self
                .attrs_filters
                .iter()
                .map(|(col, _)| format!("{col} = ?"))
                .collect();
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
            for (_, v) in &self.attrs_filters {
                values.push(crate::eloquent::model::json_value_to_sea_value(v));
            }
        }
        sql.push_str(" LIMIT 1");
        let stmt = Statement::from_sql_and_values(backend, &sql, values);
        let row = <<M as EloquentModel>::Entity as sea_orm::EntityTrait>::Model::find_by_statement(
            stmt,
        )
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(row.map(M::from))
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
}
