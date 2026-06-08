//! Tri-state field type that distinguishes "absent from payload" from
//! "explicit null" from "value provided". Required for PATCH endpoints
//! where the absent-vs-null distinction has semantic meaning.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Three-state field for partial-update payloads.
///
/// - `Absent` — key was not present in the input JSON.
/// - `Null` — key was present with an explicit `null` value.
/// - `Value(T)` — key was present with a typed value.
///
/// Pair with `#[serde(default, skip_serializing_if = "Field::is_absent")]`
/// on the struct field to wire absent-detection on deserialize and
/// absent-omission on serialize.
///
/// # Caveat: `Field<Option<T>>` is lossy
///
/// Stacking `Field` over `Option` collapses one state during a JSON
/// round-trip. `Field::Value(None)` serializes to `null` and deserializes
/// back to `Field::Null` — the `Value(None)` distinction is lost. For
/// "absent vs explicit null" semantics over an inner-nullable column,
/// model the column as `T` and let `Field::Null` carry the "clear it"
/// signal directly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum Field<T> {
    /// Field was not present in the input at all (key absent on the wire).
    #[default]
    Absent,
    /// Field was present in the input with an explicit `null` value.
    Null,
    /// Field was present and carried a value.
    Value(T),
}

impl<T> Field<T> {
    /// Returns `true` when the field was absent from the input.
    pub fn is_absent(&self) -> bool {
        matches!(self, Field::Absent)
    }

    /// Returns `true` when the field was explicitly null.
    pub fn is_null(&self) -> bool {
        matches!(self, Field::Null)
    }

    /// Returns `true` when the field carried a value.
    pub fn is_value(&self) -> bool {
        matches!(self, Field::Value(_))
    }

    /// Borrow the inner value when present; `None` for `Absent` or `Null`.
    pub fn as_value(&self) -> Option<&T> {
        match self {
            Field::Value(v) => Some(v),
            _ => None,
        }
    }

    /// Consume the field and yield the inner value when present; `None` for `Absent` or `Null`.
    pub fn into_value(self) -> Option<T> {
        match self {
            Field::Value(v) => Some(v),
            _ => None,
        }
    }

    /// Convert into `Option<Option<T>>` for endpoints that need to distinguish
    /// "do not touch this field" (`None`) from "set this field to null"
    /// (`Some(None)`) from "set this field to a value" (`Some(Some(v))`).
    ///
    /// This is the typical PATCH-against-a-DB-column pattern.
    pub fn into_option_or_null(self) -> Option<Option<T>> {
        match self {
            Field::Absent => None,
            Field::Null => Some(None),
            Field::Value(v) => Some(Some(v)),
        }
    }
}

impl<T> From<T> for Field<T> {
    fn from(v: T) -> Self {
        Field::Value(v)
    }
}

/// `Some(v)` → `Field::Value(v)`. `None` → `Field::Null` (NOT `Absent`).
///
/// Rationale: the `Option` itself was present in the caller's data flow;
/// it just happened to carry no value. That maps to "present and null."
/// Use `Field::Absent` directly when you want to signal "not provided."
impl<T> From<Option<T>> for Field<T> {
    fn from(o: Option<T>) -> Self {
        match o {
            Some(v) => Field::Value(v),
            None => Field::Null,
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Field<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // serde calls `deserialize` only when the key IS present (because
        // we pair this with `#[serde(default)]` at the field site).
        // Inside this call we only need to disambiguate null vs value.
        let opt: Option<T> = Option::deserialize(d)?;
        Ok(match opt {
            Some(v) => Field::Value(v),
            None => Field::Null,
        })
    }
}

impl<T: Serialize> Serialize for Field<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            // Absent serializes as null when emitted; pair with
            // `skip_serializing_if = "Field::is_absent"` at the field site
            // to omit the key entirely.
            Field::Absent | Field::Null => s.serialize_none(),
            Field::Value(v) => s.serialize_some(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_absent() {
        let f: Field<String> = Field::default();
        assert!(f.is_absent());
    }

    #[test]
    fn helpers() {
        let v: Field<i32> = Field::Value(7);
        assert_eq!(v.as_value(), Some(&7));
        assert_eq!(v.clone().into_value(), Some(7));
        assert!(!v.is_absent());
        assert!(!v.is_null());
        assert!(v.is_value());
    }

    // --- into_value ---

    #[test]
    fn into_value_returns_some_for_value() {
        let f: Field<i32> = Field::Value(7);
        assert_eq!(f.into_value(), Some(7));
    }

    #[test]
    fn into_value_returns_none_for_absent_or_null() {
        let f: Field<i32> = Field::Absent;
        assert_eq!(f.into_value(), None);
        let f: Field<i32> = Field::Null;
        assert_eq!(f.into_value(), None);
    }

    // --- into_option_or_null ---

    #[test]
    fn into_option_or_null_three_way() {
        let absent: Field<i32> = Field::Absent;
        assert_eq!(absent.into_option_or_null(), None);

        let null: Field<i32> = Field::Null;
        assert_eq!(null.into_option_or_null(), Some(None));

        let value: Field<i32> = Field::Value(7);
        assert_eq!(value.into_option_or_null(), Some(Some(7)));
    }

    // --- From<Option<T>> ---

    #[test]
    fn from_some_option_yields_value() {
        let f: Field<i32> = Some(7).into();
        assert_eq!(f, Field::Value(7));
    }

    #[test]
    fn from_none_option_yields_null_not_absent() {
        let f: Field<i32> = None.into();
        assert_eq!(f, Field::Null);
        assert!(!f.is_absent());
    }

    #[test]
    fn from_t_yields_value() {
        let f: Field<i32> = 7.into();
        assert_eq!(f, Field::Value(7));
    }

    // --- Field<Option<T>> lossy round-trip (known limitation) ---

    #[test]
    fn field_of_option_collapses_value_none_to_null_round_trip() {
        use serde::{Deserialize, Serialize};

        #[derive(Debug, Deserialize, Serialize, PartialEq)]
        struct Holder {
            #[serde(default, skip_serializing_if = "Field::is_absent")]
            x: Field<Option<String>>,
        }

        let with_value_none = Holder {
            x: Field::Value(None),
        };
        let json = serde_json::to_string(&with_value_none).unwrap();
        assert_eq!(json, r#"{"x":null}"#);

        let back: Holder = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.x,
            Field::Null,
            "Value(None) round-trips through JSON as Null — known limitation"
        );
    }
}
