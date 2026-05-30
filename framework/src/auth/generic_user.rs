//! A schemaless authenticated user — Laravel's `GenericUser`.
//!
//! Returned by [`DatabaseUserProvider`](super::database_provider::DatabaseUserProvider),
//! which authenticates against a raw table rather than a typed model.
//! It carries the full row as a JSON attribute map plus the resolved id
//! and (optional) password hash.

use std::any::Any;

use serde_json::{Map, Value};

use super::authenticatable::Authenticatable;

/// A user backed by a raw database row rather than a typed model.
///
/// Holds every column of the row in [`attributes`](Self::attributes),
/// with the id and password hash pulled out for the [`Authenticatable`]
/// contract.
#[derive(Debug, Clone)]
pub struct GenericUser {
    id: String,
    password: Option<String>,
    attributes: Map<String, Value>,
}

impl GenericUser {
    /// Build a generic user from its resolved id, optional password hash,
    /// and the full row attribute map.
    pub fn new(
        id: impl Into<String>,
        password: Option<String>,
        attributes: Map<String, Value>,
    ) -> Self {
        Self {
            id: id.into(),
            password,
            attributes,
        }
    }

    /// Read a column from the underlying row by name.
    pub fn attribute(&self, key: &str) -> Option<&Value> {
        self.attributes.get(key)
    }

    /// The full row as a JSON object.
    pub fn attributes(&self) -> &Map<String, Value> {
        &self.attributes
    }
}

impl Authenticatable for GenericUser {
    /// Returns the original string id — `GenericUser`'s identifier
    /// column is stored as a string, so the canonical
    /// [`Authenticatable::get_auth_identifier`] surface is a clone of
    /// the stored value. The optional integer form falls back to the
    /// trait default, which parses this string (returning `0` for
    /// non-numeric ids like UUIDs and opaque torii ids).
    fn get_auth_identifier(&self) -> String {
        self.id.clone()
    }

    fn get_auth_password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("id".into(), Value::from(7));
        m.insert("email".into(), Value::from("a@b.com"));
        m.insert("password".into(), Value::from("$2$hash"));
        m
    }

    #[test]
    fn exposes_id_password_and_attributes() {
        let u = GenericUser::new("7", Some("$2$hash".into()), row());
        assert_eq!(u.get_auth_identifier(), "7");
        assert_eq!(u.auth_identifier(), 7);
        assert_eq!(u.get_auth_password(), Some("$2$hash"));
        assert_eq!(
            u.attribute("email").and_then(|v| v.as_str()),
            Some("a@b.com")
        );
    }

    #[test]
    fn non_numeric_id_falls_back_to_zero_for_int_form() {
        let u = GenericUser::new("usr_abc", None, Map::new());
        assert_eq!(u.get_auth_identifier(), "usr_abc");
        assert_eq!(u.auth_identifier(), 0);
        assert_eq!(u.get_auth_password(), None);
    }
}
