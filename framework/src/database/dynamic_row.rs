//! Phase 10C T10 — `DynamicRow` newtype over `serde_json::Map`.
//!
//! Returned by [`DB::table(name)`](crate::DB::table) and
//! [`DB::select`](crate::DB::select) for the model-less escape hatch:
//! tables that aren't worth a full `#[suprnova::model]` (audit logs,
//! reporting joins, ad-hoc dashboards). Carries the row's columns as
//! `serde_json::Value` and exposes typed accessors that return
//! [`Result`] with a clear error message when the column is missing or
//! the runtime type doesn't match.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use suprnova::DB;
//!
//! let rows = DB::table("audit_log")
//!     .filter_op("actor_id", ">", 0)
//!     .order_by_desc("id")
//!     .limit(50)
//!     .get()
//!     .await?;
//!
//! for row in rows.iter() {
//!     let event: String = row.get_string("event")?;
//!     let actor_id: i64 = row.get_int("actor_id")?;
//!     let score: Option<i64> = row.get_optional_int("score")?;
//!     // ...
//! }
//! ```
//!
//! ## Missing key vs null value
//!
//! The `get_*` family returns `Err(FrameworkError::param)` when the
//! column is absent from the row. `get_optional_*` distinguishes:
//!
//! - **Column missing** → `Err` (schema mismatch — the caller asked
//!   for a column the query didn't select).
//! - **Column present, value null** → `Ok(None)` (nullable column
//!   semantics — the column exists but the row carries SQL NULL).
//! - **Column present, value typed** → `Ok(Some(_))`.
//!
//! Use the typed `get_*` for non-nullable columns and `get_optional_*`
//! for nullable ones. Both bail with a clear error on type mismatch.

use crate::FrameworkError;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// A row returned by the [`DB::table`](crate::DB::table) builder or
/// [`DB::select`](crate::DB::select) raw escape hatch. Stores columns
/// as a `serde_json::Map<String, Value>` and exposes typed accessors.
///
/// See the [module docs](self) for the missing-key vs null-value
/// contract and an end-to-end usage example.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct DynamicRow(pub Map<String, Value>);

impl DynamicRow {
    /// Wrap a `Map<String, Value>` as a `DynamicRow`. Used by
    /// `DB::table` after `JsonValue::find_by_statement` materialises
    /// each row into an `Object(Map)`.
    pub fn from_map(m: Map<String, Value>) -> Self {
        Self(m)
    }

    /// Consume the row and return the inner map. Useful when you want
    /// to merge the row with extra fields before re-serialising.
    pub fn into_map(self) -> Map<String, Value> {
        self.0
    }

    /// Read an `i64` column. Errors if the column is missing or the
    /// stored value is not an integer.
    pub fn get_int(&self, key: &str) -> Result<i64, FrameworkError> {
        let v = self
            .0
            .get(key)
            .ok_or_else(|| FrameworkError::param(format!("column '{key}' not found in row")))?;
        v.as_i64()
            .ok_or_else(|| FrameworkError::param(format!("column '{key}' is not an int: {v}")))
    }

    /// Read a `String` column. Errors if the column is missing or the
    /// stored value is not a string.
    pub fn get_string(&self, key: &str) -> Result<String, FrameworkError> {
        let v = self
            .0
            .get(key)
            .ok_or_else(|| FrameworkError::param(format!("column '{key}' not found in row")))?;
        v.as_str()
            .map(String::from)
            .ok_or_else(|| FrameworkError::param(format!("column '{key}' is not a string: {v}")))
    }

    /// Read a `bool` column. Errors if the column is missing or the
    /// stored value is not a boolean.
    pub fn get_bool(&self, key: &str) -> Result<bool, FrameworkError> {
        let v = self
            .0
            .get(key)
            .ok_or_else(|| FrameworkError::param(format!("column '{key}' not found in row")))?;
        v.as_bool()
            .ok_or_else(|| FrameworkError::param(format!("column '{key}' is not a bool: {v}")))
    }

    /// Read a `f64` column. Errors if the column is missing or the
    /// stored value is not a number. Integer values are accepted and
    /// coerced (matches `serde_json::Value::as_f64`).
    pub fn get_float(&self, key: &str) -> Result<f64, FrameworkError> {
        let v = self
            .0
            .get(key)
            .ok_or_else(|| FrameworkError::param(format!("column '{key}' not found in row")))?;
        v.as_f64()
            .ok_or_else(|| FrameworkError::param(format!("column '{key}' is not a number: {v}")))
    }

    /// Read the raw JSON value for a column. Useful when you want the
    /// `serde_json::Value` directly (e.g. nested objects, arrays).
    /// Returns a clone — the row stays usable.
    pub fn get_value(&self, key: &str) -> Result<Value, FrameworkError> {
        self.0
            .get(key)
            .cloned()
            .ok_or_else(|| FrameworkError::param(format!("column '{key}' not found in row")))
    }

    /// Deserialise a column into any `T: DeserializeOwned`. The full
    /// `serde_json` deserialisation surface is available — `T` can be a
    /// `Vec<U>`, a `HashMap<K, V>`, a `chrono::DateTime`, or a
    /// user-defined struct with `#[derive(Deserialize)]`.
    ///
    /// Errors wrap `serde_json` deserialisation errors with the column
    /// name attached for debuggability.
    pub fn get_as<T: DeserializeOwned>(&self, key: &str) -> Result<T, FrameworkError> {
        let v = self.get_value(key)?;
        serde_json::from_value(v)
            .map_err(|e| FrameworkError::param(format!("column '{key}' deserialise: {e}")))
    }

    /// Read a nullable string column. Returns `Ok(None)` when the
    /// column is present and the value is SQL NULL; `Err` when the
    /// column is missing entirely (see [module docs](self) for the
    /// missing-key vs null-value contract).
    pub fn get_optional_string(&self, key: &str) -> Result<Option<String>, FrameworkError> {
        match self.0.get(key) {
            None => Err(FrameworkError::param(format!(
                "column '{key}' not found in row"
            ))),
            Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(v) => Err(FrameworkError::param(format!(
                "column '{key}' is not a string: {v}"
            ))),
        }
    }

    /// Read a nullable integer column. Same contract as
    /// [`Self::get_optional_string`] — missing column errors, SQL NULL
    /// returns `Ok(None)`.
    pub fn get_optional_int(&self, key: &str) -> Result<Option<i64>, FrameworkError> {
        match self.0.get(key) {
            None => Err(FrameworkError::param(format!(
                "column '{key}' not found in row"
            ))),
            Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_i64()
                .map(Some)
                .ok_or_else(|| FrameworkError::param(format!("column '{key}' is not an int: {v}"))),
        }
    }
}

impl std::ops::Deref for DynamicRow {
    type Target = Map<String, Value>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn from_map_round_trip() {
        let mut m = Map::new();
        m.insert("k".into(), json!(1));
        let row = DynamicRow::from_map(m.clone());
        assert_eq!(row.into_map(), m);
    }

    #[test]
    fn get_int_string_bool_value_work() {
        let mut m = Map::new();
        m.insert("i".into(), json!(7));
        m.insert("s".into(), json!("hi"));
        m.insert("b".into(), json!(false));
        m.insert("j".into(), json!({"k": "v"}));
        let row = DynamicRow::from_map(m);

        assert_eq!(row.get_int("i").unwrap(), 7);
        assert_eq!(row.get_string("s").unwrap(), "hi");
        assert!(!row.get_bool("b").unwrap());
        assert_eq!(row.get_value("j").unwrap(), json!({"k": "v"}));
    }

    #[test]
    fn get_optional_distinguishes_missing_from_null() {
        let mut m = Map::new();
        m.insert("present_null".into(), Value::Null);
        m.insert("present_val".into(), json!("x"));
        let row = DynamicRow::from_map(m);

        // Present + null → Ok(None)
        assert_eq!(row.get_optional_string("present_null").unwrap(), None);
        // Present + value → Ok(Some)
        assert_eq!(
            row.get_optional_string("present_val").unwrap(),
            Some("x".to_string())
        );
        // Missing → Err
        assert!(row.get_optional_string("missing").is_err());
    }

    #[test]
    fn get_as_deserialises_struct() {
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Prefs {
            theme: String,
        }

        let mut m = Map::new();
        m.insert("p".into(), json!({"theme": "dark"}));
        let row = DynamicRow::from_map(m);

        let p: Prefs = row.get_as("p").unwrap();
        assert_eq!(p.theme, "dark");
    }

    #[test]
    fn deref_exposes_map_iteration() {
        let mut m = Map::new();
        m.insert("a".into(), json!(1));
        m.insert("b".into(), json!(2));
        let row = DynamicRow::from_map(m);

        let keys: Vec<&String> = row.keys().collect();
        assert_eq!(keys.len(), 2);
    }
}
