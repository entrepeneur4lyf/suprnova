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

use chrono::Utc;

use super::authenticatable::Authenticatable;
use super::must_verify_email::{AuthFlowUser, CanResetPassword, MustVerifyEmail};
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

impl<M> EloquentUserProvider<M>
where
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
    /// Load the typed model `M` by id, using the same lookup column and
    /// id-binding as [`retrieve_by_id`](UserProvider::retrieve_by_id). Shared
    /// by the auth-flow methods that need the concrete `M` (not a trait
    /// object) so they can read [`MustVerifyEmail`] fields and `save()`.
    async fn find_by_identifier(&self, id: &str) -> Result<Option<M>, FrameworkError> {
        let column = self
            .identifier_column
            .clone()
            .unwrap_or_else(|| M::primary_key_name().to_string());
        M::query()
            .filter(column, (self.id_parser)(id))
            .first()
            .await
    }
}

#[async_trait]
impl<M> UserProvider for EloquentUserProvider<M>
where
    // The Model trait's where-clause does not auto-propagate to an
    // `M: Model` bound, and `Builder::first` adds `EagerLoadDispatch` +
    // `FromQueryResult`. Restated here to match `Builder`'s terminal
    // impl block (`Self` → `M`).
    M: Model
        + Authenticatable
        + MustVerifyEmail
        + CanResetPassword
        + From<<M::Entity as EntityTrait>::Model>
        + EagerLoadDispatch,
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
        let user = self.find_by_identifier(id).await?;
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
            // A password was supplied but the matched account is
            // passwordless. Returning `Ok(false)` here with no hash work
            // would fingerprint "account exists but is passwordless": the
            // unknown-identifier and wrong-password paths both run a
            // fixed-cost verify (the latter the real one, the former via
            // `dummy_verify`), while this branch would short-circuit. Run
            // the same dummy verify so all three paths cost the same.
            (Some(_), None) => {
                self.dummy_verify().await?;
                Ok(false)
            }
            // No password supplied at all — nothing to equalise against a
            // real verify, so no dummy work is warranted.
            (None, _) => Ok(false),
        }
    }

    async fn retrieve_by_email(&self, email: &str) -> Result<Option<AuthFlowUser>, FrameworkError> {
        let user = M::query().filter("email", email).first().await?;
        // This is the lookup BY email: the caller already supplied the target
        // address, so echo the queried `email` back into the carrier — it IS
        // the verify/reset target the user typed.
        Ok(user.map(|u| AuthFlowUser {
            id: u.get_auth_identifier(),
            email: email.to_string(),
            name: u.name().map(str::to_string),
        }))
    }

    async fn flow_user_by_id(&self, id: &str) -> Result<Option<AuthFlowUser>, FrameworkError> {
        let user = self.find_by_identifier(id).await?;
        // This path exists only to address the password-changed mail in the
        // reset flow, so source the address from `email_for_reset()` (the
        // `CanResetPassword` contract) rather than the verification email.
        Ok(user.map(|u| AuthFlowUser {
            id: u.get_auth_identifier(),
            email: u.email_for_reset().to_string(),
            name: u.name().map(str::to_string),
        }))
    }

    async fn mark_email_verified(&self, id: &str) -> Result<(), FrameworkError> {
        // load → mutate → `Model::save`: this (a) fires the full model
        // lifecycle (Saving/Updating/Updated/Saved) so observers and audit see
        // the change, and (b) is a read-modify-write of the whole row, so a
        // concurrent flow on the same user could clobber a field — acceptable
        // here as both verify/reset paths are token-gated. Absent id → no-op.
        if let Some(mut user) = self.find_by_identifier(id).await? {
            user.set_email_verified_at(Some(Utc::now()));
            <M as Model>::save(&user).await?;
        }
        Ok(())
    }

    async fn set_password(&self, id: &str, hashed: &str) -> Result<(), FrameworkError> {
        // load → mutate → `Model::save`: this (a) fires the full model
        // lifecycle (Saving/Updating/Updated/Saved) so observers and audit see
        // the change, and (b) is a read-modify-write of the whole row, so a
        // concurrent flow on the same user could clobber a field — acceptable
        // here as both verify/reset paths are token-gated. Absent id → no-op.
        if let Some(mut user) = self.find_by_identifier(id).await? {
            // `hashed` arrives ALREADY HASHED — store it verbatim. `save()`
            // overlays the full serialized model onto the ActiveModel, so the
            // password column persists regardless of fillable/guarded.
            user.set_password_hash(hashed);
            <M as Model>::save(&user).await?;
        }
        Ok(())
    }

    async fn is_email_verified(&self, id: &str) -> Result<bool, FrameworkError> {
        Ok(self
            .find_by_identifier(id)
            .await?
            .map(|u| u.is_email_verified())
            .unwrap_or(false))
    }
}
