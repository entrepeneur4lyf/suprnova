//! Model-backed user provider — Laravel's `EloquentUserProvider`.
//!
//! Resolves users through a typed [`Model`] that
//! also implements [`Authenticatable`]. The typed half of the
//! transparency lever: an app whose `User` is a `#[suprnova::model]`
//! registers `EloquentUserProvider::<User>::new()` and needs no
//! hand-written [`UserProvider`].
//!
//! ```rust,ignore
//! // In bootstrap.rs (User: Model + Authenticatable):
//! Auth::register_provider("users", Arc::new(EloquentUserProvider::<User>::new()))?;
//! ```
//!
//! Shares the [`DatabaseUserProvider`](super::database_provider::DatabaseUserProvider)
//! security posture: `retrieve_by_credentials` filters only on the
//! configured credential-column allowlist, never on arbitrary keys in
//! the credential map.

use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;
use sea_orm::{EntityTrait, FromQueryResult, IntoActiveModel, PrimaryKeyTrait};
use serde::Serialize;
use serde_json::Value;

use super::authenticatable::Authenticatable;
use super::provider::UserProvider;
use crate::eloquent::{EagerLoadDispatch, Model};
use crate::error::FrameworkError;
use crate::hashing;

/// The default id binder: numeric ids bind as integers (matching integer
/// primary keys), everything else binds as a string. See
/// [`DatabaseUserProvider::with_id_parser`](super::database_provider::DatabaseUserProvider::with_id_parser)
/// for the zero-padded-string-PK caveat.
fn default_id_parser(id: &str) -> Value {
    match id.parse::<i64>() {
        Ok(n) => Value::from(n),
        Err(_) => Value::from(id),
    }
}

/// A [`UserProvider`] that resolves users from a typed model `M`.
///
/// `M` must be both a [`Model`] (for querying)
/// and [`Authenticatable`] (for the id / password contract).
pub struct EloquentUserProvider<M> {
    /// The lookup column for `retrieve_by_id`. `None` uses the model's
    /// primary key.
    identifier_column: Option<String>,
    /// The credential-lookup allowlist (default `["email"]`).
    credential_columns: Vec<String>,
    id_parser: fn(&str) -> Value,
    // `fn() -> M` so the marker is Send + Sync + covariant regardless of
    // M, and does not imply ownership of an M.
    _marker: PhantomData<fn() -> M>,
}

impl<M> EloquentUserProvider<M> {
    /// A provider for model `M`, looking up by primary key for ids and by
    /// `email` for credentials.
    pub fn new() -> Self {
        Self {
            identifier_column: None,
            credential_columns: vec!["email".to_string()],
            id_parser: default_id_parser,
            _marker: PhantomData,
        }
    }

    /// Override the `retrieve_by_id` lookup column (defaults to the model's
    /// primary key).
    pub fn identifier_column(mut self, column: impl Into<String>) -> Self {
        self.identifier_column = Some(column.into());
        self
    }

    /// Set the credential-lookup allowlist (default `["email"]`). Only
    /// these columns can become `WHERE` predicates in
    /// `retrieve_by_credentials`.
    pub fn credential_columns<I, S>(mut self, columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.credential_columns = columns.into_iter().map(Into::into).collect();
        self
    }

    /// Override how a string id is bound into the SQL lookup.
    pub fn with_id_parser(mut self, parser: fn(&str) -> Value) -> Self {
        self.id_parser = parser;
        self
    }
}

impl<M> Default for EloquentUserProvider<M> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<M> UserProvider for EloquentUserProvider<M>
where
    // The Model trait's where-clause does not auto-propagate to an
    // `M: Model` bound, and `Builder::first` adds `EagerLoadDispatch` +
    // `FromQueryResult`. Restated here to match `Builder`'s terminal
    // impl block (`Self` → `M`).
    M: Model + Authenticatable + From<<M::Entity as EntityTrait>::Model> + EagerLoadDispatch,
    <M::Entity as EntityTrait>::Model: From<M>
        + IntoActiveModel<<M::Entity as EntityTrait>::ActiveModel>
        + FromQueryResult
        + Serialize
        + Send
        + Sync,
    <M::Entity as EntityTrait>::ActiveModel: Send,
    <<M::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    async fn retrieve_by_id(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        let column = self
            .identifier_column
            .clone()
            .unwrap_or_else(|| M::primary_key_name().to_string());
        let user = M::query()
            .filter(column, (self.id_parser)(id))
            .first()
            .await?;
        Ok(user.map(|u| Arc::new(u) as Arc<dyn Authenticatable>))
    }

    async fn retrieve_by_credentials(
        &self,
        credentials: &Value,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        let object = match credentials.as_object() {
            Some(o) => o,
            None => return Ok(None),
        };

        let mut query = M::query();
        let mut matched_any = false;
        for column in &self.credential_columns {
            // The password is verified separately and never used as a lookup
            // filter.
            if column == "password" {
                continue;
            }
            if let Some(value) = object.get(column) {
                query = query.filter(column.clone(), value.clone());
                matched_any = true;
            }
        }

        if !matched_any {
            return Ok(None);
        }

        let user = query.first().await?;
        Ok(user.map(|u| Arc::new(u) as Arc<dyn Authenticatable>))
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
