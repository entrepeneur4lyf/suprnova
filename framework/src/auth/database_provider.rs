//! Table-backed user provider — Laravel's `DatabaseUserProvider`.
//!
//! Authenticates against a raw table via [`crate::DB::table`], returning
//! a [`GenericUser`]. The fully-generic provider: no model type, no
//! macro — point it at a table and it works, so the common case needs no
//! hand-written [`UserProvider`].
//!
//! ```rust,ignore
//! // In bootstrap.rs:
//! Auth::register_provider("users", Arc::new(DatabaseUserProvider::new("users")))?;
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::Value as SeaValue;
use serde_json::Value;

use super::authenticatable::Authenticatable;
use super::generic_user::GenericUser;
use super::provider::UserProvider;
use crate::database::{DB, DynamicRow};
use crate::error::FrameworkError;
use crate::hashing;

/// The default id binder: numeric ids bind as integers (matching integer
/// primary keys), everything else binds as a string (UUIDs, opaque ids).
///
/// Trade-off: a zero-padded string id like `"007"` parses to the integer
/// `7` and would mis-bind against a *text* primary key. Apps with such
/// keys override the binder with [`DatabaseUserProvider::with_id_parser`].
fn default_id_parser(id: &str) -> SeaValue {
    match id.parse::<i64>() {
        Ok(n) => SeaValue::from(n),
        Err(_) => SeaValue::from(id.to_string()),
    }
}

/// Convert a JSON credential value into a SQL bind.
fn json_to_sea_value(v: &Value) -> SeaValue {
    match v {
        Value::String(s) => SeaValue::from(s.clone()),
        Value::Bool(b) => SeaValue::from(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SeaValue::from(i)
            } else if let Some(f) = n.as_f64() {
                SeaValue::from(f)
            } else {
                SeaValue::from(n.to_string())
            }
        }
        other => SeaValue::from(other.to_string()),
    }
}

/// Stringify a JSON value for the id field (numbers become their decimal
/// form; strings pass through).
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A [`UserProvider`] that resolves users from a raw database table.
///
/// Configure the table and (optionally) the id / password / credential
/// columns. `retrieve_by_credentials` filters **only** on the configured
/// [`credential_columns`](Self::credential_columns) allowlist, so extra
/// keys in an attacker-influenced credential map can never inject extra
/// `WHERE` predicates.
pub struct DatabaseUserProvider {
    table: String,
    identifier_column: String,
    password_column: String,
    credential_columns: Vec<String>,
    id_parser: fn(&str) -> SeaValue,
}

impl DatabaseUserProvider {
    /// A provider for `table`, with `id` / `password` columns and an
    /// `email` credential lookup by default.
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            identifier_column: "id".to_string(),
            password_column: "password".to_string(),
            credential_columns: vec!["email".to_string()],
            id_parser: default_id_parser,
        }
    }

    /// Set the primary-key / id column (default `"id"`).
    pub fn identifier_column(mut self, column: impl Into<String>) -> Self {
        self.identifier_column = column.into();
        self
    }

    /// Set the password-hash column (default `"password"`).
    pub fn password_column(mut self, column: impl Into<String>) -> Self {
        self.password_column = column.into();
        self
    }

    /// Set the credential-lookup allowlist (default `["email"]`).
    ///
    /// `retrieve_by_credentials` filters on the intersection of these
    /// columns and the supplied credential keys — and nothing else. To
    /// allow login by email *or* username, pass `["email", "username"]`.
    pub fn credential_columns<I, S>(mut self, columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.credential_columns = columns.into_iter().map(Into::into).collect();
        self
    }

    /// Override how a string id is bound into the SQL lookup. The
    /// default parses numeric ids as integers and treats anything else
    /// (UUIDs, ULIDs, zero-padded codes) as a string — pass a custom
    /// parser when a string PK happens to look numeric (e.g. zero-padded
    /// `"007"`) so it doesn't bind as `7`.
    pub fn with_id_parser(mut self, parser: fn(&str) -> SeaValue) -> Self {
        self.id_parser = parser;
        self
    }

    /// Build a [`GenericUser`] from a row.
    fn row_to_user(&self, row: DynamicRow) -> Arc<dyn Authenticatable> {
        let map = row.into_map();
        let id = map
            .get(&self.identifier_column)
            .map(value_to_string)
            .unwrap_or_default();
        let password = map
            .get(&self.password_column)
            .and_then(|v| v.as_str().map(String::from));
        Arc::new(GenericUser::new(id, password, map))
    }
}

#[async_trait]
impl UserProvider for DatabaseUserProvider {
    async fn retrieve_by_id(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        let row = DB::table(&self.table)
            .filter(self.identifier_column.clone(), (self.id_parser)(id))
            .first()
            .await?;
        Ok(row.map(|r| self.row_to_user(r)))
    }

    async fn retrieve_by_credentials(
        &self,
        credentials: &Value,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        let object = match credentials.as_object() {
            Some(o) => o,
            None => return Ok(None),
        };

        let mut query = DB::table(&self.table);
        let mut matched_any = false;
        for column in &self.credential_columns {
            // The password is verified separately and never used as a lookup
            // filter, even if it appears in the allowlist by mistake.
            if column == &self.password_column {
                continue;
            }
            if let Some(value) = object.get(column) {
                query = query.filter(column.clone(), json_to_sea_value(value));
                matched_any = true;
            }
        }

        // No allowlisted credential present → nothing to look up. This also
        // stops a bare `{}` (or a body carrying only non-allowlisted keys)
        // from matching the first row in the table.
        if !matched_any {
            return Ok(None);
        }

        let row = query.first().await?;
        Ok(row.map(|r| self.row_to_user(r)))
    }

    async fn validate_credentials(
        &self,
        user: &dyn Authenticatable,
        credentials: &Value,
    ) -> Result<bool, FrameworkError> {
        let password = credentials.get("password").and_then(|v| v.as_str());
        match (password, user.get_auth_password()) {
            (Some(plaintext), Some(hash)) => hashing::verify_async(plaintext, hash).await,
            _ => Ok(false),
        }
    }
}
