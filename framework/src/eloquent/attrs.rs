//! Typed JSON map used for mass-assignment surfaces (`create`,
//! `update`, `first_or_create`, etc.).
//!
//! `Attrs` preserves insertion order (`IndexMap`-backed) so SQL UPDATE
//! statements list columns in the same order the caller passed them —
//! a small win for human-readable logs and snapshot tests.
//!
//! The companion [`attrs!`](crate::attrs) declarative macro is the
//! ergonomic entry point: it stringifies identifier keys at compile
//! time and uses `serde_json::json!` to coerce values, so all of these
//! work:
//!
//! ```rust,no_run
//! use suprnova::attrs;
//!
//! let a = attrs! { name: "Alice", email: "a@example.com" };
//! let b = attrs! { age: 32, active: true };
//! let c = attrs! { tags: vec!["x", "y"], scores: [1, 2, 3] };
//! ```

use indexmap::IndexMap;
use serde_json::Value;

/// An ordered map of column-name to JSON value, used by every
/// mass-assignment entry point. Built via the [`attrs!`](crate::attrs)
/// macro.
#[derive(Debug, Clone, Default)]
pub struct Attrs(pub IndexMap<String, Value>);

impl Attrs {
    /// Construct an empty `Attrs`. Identical to `Attrs::default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a single column. Used by the `attrs!` macro and by
    /// hand-built `Attrs` instances. Overwrites any existing entry for
    /// the same key while preserving the original insertion position
    /// (`IndexMap` semantics).
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) -> &mut Self {
        self.0.insert(key.into(), value.into());
        self
    }

    /// Read a column's raw JSON value, if present.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// Iterate over the column names in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(|s| s.as_str())
    }

    /// Iterate over `(name, value)` pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Number of columns set.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no columns are set.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns whether the given column name is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// Consume `other` and overlay it onto `self`. Used by
    /// `first_or_create` and `update_or_create` to combine the lookup
    /// columns with the "extras". `other`'s values win on overlap.
    pub fn merge(mut self, other: Attrs) -> Self {
        for (k, v) in other.0 {
            self.0.insert(k, v);
        }
        self
    }
}

impl From<Value> for Attrs {
    fn from(value: Value) -> Self {
        match value {
            Value::Object(map) => {
                let mut indexed = IndexMap::with_capacity(map.len());
                for (k, v) in map {
                    indexed.insert(k, v);
                }
                Attrs(indexed)
            }
            _ => Attrs::default(),
        }
    }
}

/// Build an [`Attrs`] map from key-value pairs with `serde_json::json!`-style
/// value coercion.
///
/// Keys are bare identifiers and stringified at compile time. Values
/// pass through `serde_json::json!` so anything `Serialize` works
/// directly (numbers, strings, bools, `Vec<T>`, arrays).
///
/// ```rust,no_run
/// use suprnova::attrs;
///
/// let attrs = attrs! {
///     name: "Alice",
///     email: "a@example.com",
///     age: 32,
/// };
/// assert_eq!(attrs.len(), 3);
/// ```
#[macro_export]
macro_rules! attrs {
    () => { $crate::eloquent::Attrs::new() };
    ($($key:ident: $value:expr),* $(,)?) => {{
        let mut a = $crate::eloquent::Attrs::new();
        $( a.insert(stringify!($key), $crate::serde_json::json!($value)); )*
        a
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attrs_macro_builds_ordered_map() {
        let a = attrs! { name: "Alice", email: "alice@example.com", age: 32 };
        let keys: Vec<&str> = a.keys().collect();
        assert_eq!(keys, vec!["name", "email", "age"]);
    }

    #[test]
    fn attrs_macro_supports_empty() {
        let a = attrs! {};
        assert!(a.is_empty());
    }

    #[test]
    fn attrs_merge_overlays_other() {
        let base = attrs! { name: "Old", email: "a@x.com" };
        let extra = attrs! { name: "New" };
        let merged = base.merge(extra);
        assert_eq!(merged.get("name").unwrap().as_str().unwrap(), "New");
        assert_eq!(merged.get("email").unwrap().as_str().unwrap(), "a@x.com");
    }

    #[test]
    fn attrs_from_value_object() {
        let v = serde_json::json!({ "a": 1, "b": "two" });
        let a = Attrs::from(v);
        assert_eq!(a.len(), 2);
    }
}
