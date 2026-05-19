//! Eloquent query builder primitives.
//!
//! Phase 10A T3 ships the `IntoColumn` trait — the bridge that lets
//! later `Builder<M>` methods (`filter`, `db_where`, `order_by`, ...)
//! accept either typed `Column` variants or string column names. The
//! macro-emitted `Column` enums impl `IntoColumn` directly; `&str` and
//! `String` impl it via runtime lookup against the model's `Column::from_name`.
//!
//! Task 4 builds the full `Builder<M>` surface on top of this trait.
//! Task 5 wires the dual-API methods (`filter` / `db_where`,
//! `dispatch` / `enqueue`, ...) that consume `impl IntoColumn`.
//!
//! The trait is intentionally tiny here: the macro-generated `Column`
//! type is the only place we know enough at compile time to convert
//! to a name without going through a string lookup. Builder methods
//! that need additional column-level information (e.g. the SeaORM
//! `ColumnTrait` instance) will layer on top in T4.
//!
//! The trait lives in its own module so future builder methods that
//! need additional bound-on-column trait operations have a stable home
//! to grow into.
//!
//! Surface visibility is `pub`: this is part of the user-facing
//! re-export path `suprnova::eloquent::builder::IntoColumn`. The
//! macro emits qualified paths against that location, so consumers
//! who write their own builder-style helpers can target it directly.
//!
//! Type-bridge invariant: `Column::from_name("not_a_column")` returns
//! `None`. The string-based impls below return an empty string on
//! that path; T4's `Builder` is responsible for catching the empty
//! string and producing a user-friendly error. Empty here is the
//! safe default — SeaORM will refuse to build SQL against an empty
//! column name and the failure surfaces immediately.

/// Convert a value into a column name for use with Eloquent's
/// `Builder<M>` methods. Implemented by every macro-generated `Column`
/// enum so users can write either typed (`Column::Email`) or string
/// (`"email"`) arguments throughout the builder API.
pub trait IntoColumn {
    /// Return the snake-case column name as a `String`. Owned because
    /// the typed-enum impl materialises a new string from a `&'static
    /// str` accessor.
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
