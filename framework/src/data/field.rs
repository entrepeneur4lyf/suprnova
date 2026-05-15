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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Field<T> {
    Absent,
    Null,
    Value(T),
}

impl<T> Default for Field<T> {
    fn default() -> Self {
        Field::Absent
    }
}

impl<T> Field<T> {
    pub fn is_absent(&self) -> bool {
        matches!(self, Field::Absent)
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Field::Null)
    }

    pub fn is_value(&self) -> bool {
        matches!(self, Field::Value(_))
    }

    pub fn as_value(&self) -> Option<&T> {
        match self {
            Field::Value(v) => Some(v),
            _ => None,
        }
    }

    pub fn into_value(self) -> Option<T> {
        match self {
            Field::Value(v) => Some(v),
            _ => None,
        }
    }

    /// Collapse `Absent` into `Null`. Useful for endpoints that treat both
    /// as "clear the field".
    pub fn into_option_null(self) -> Option<T> {
        match self {
            Field::Value(v) => Some(v),
            _ => None,
        }
    }
}

impl<T> From<T> for Field<T> {
    fn from(v: T) -> Self {
        Field::Value(v)
    }
}

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
}
