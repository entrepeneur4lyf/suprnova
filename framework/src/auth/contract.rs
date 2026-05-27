//! Guard contracts for the named-guard auth system.
//!
//! Mirrors Laravel's `Illuminate\Contracts\Auth\Guard` and `StatefulGuard`:
//! a [`Guard`] answers "who is the current user", and a [`StatefulGuard`] can
//! additionally log users in and out across requests. Built-in implementors
//! are the session guard and the token guard; apps reach them by name through
//! [`crate::auth::AuthManager`] / `Auth::guard("name")`.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::authenticatable::Authenticatable;
use crate::error::FrameworkError;

/// Authentication credentials — a JSON object, typically
/// `{"email": …, "password": …}` (Laravel's `array $credentials`).
///
/// ```rust,ignore
/// let creds = Credentials::password("alice@example.com", "s3cret");
/// let creds = Credentials::new().insert("username", "alice").insert("password", "s3cret");
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Credentials(pub serde_json::Map<String, serde_json::Value>);

impl Credentials {
    /// An empty credentials set.
    pub fn new() -> Self {
        Self(serde_json::Map::new())
    }

    /// The common `{"email", "password"}` pair.
    pub fn password(email: impl Into<String>, password: impl Into<String>) -> Self {
        Self::new()
            .insert("email", email.into())
            .insert("password", password.into())
    }

    /// Add a field (builder style).
    pub fn insert(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        self.0.insert(key.into(), value.into());
        self
    }

    /// Get a field as a string, if present and a string.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.as_str())
    }

    /// The credentials as a `serde_json::Value` for [`super::UserProvider`] calls.
    pub fn as_value(&self) -> serde_json::Value {
        serde_json::Value::Object(self.0.clone())
    }
}

/// A read-only authentication guard: answers who (if anyone) is authenticated.
///
/// All methods are async because non-session guards (token, database) may hit
/// IO to resolve the user. The static `Auth::check()`/`id()` fast path stays
/// synchronous for the session-backed default guard; this trait is the surface
/// behind `Auth::guard("name")`.
#[async_trait]
pub trait Guard: Send + Sync {
    /// The currently authenticated user, or `None`.
    async fn user(&self) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError>;

    /// The authenticated user's identifier, or `None`.
    async fn id(&self) -> Result<Option<String>, FrameworkError>;

    /// Validate credentials against the guard's user provider without logging in.
    async fn validate(&self, credentials: &Credentials) -> Result<bool, FrameworkError>;

    /// Set the current user for this request **without** persisting to the
    /// session — the in-memory equivalent of `once_using_id`. Mirrors
    /// Laravel's `setUser($user)` (a `GuardHelpers` method surfaced on the
    /// base contract). After this call, `user()`/`id()`/`has_user()` reflect
    /// `user` for the remainder of the request.
    async fn set_user(&self, user: Arc<dyn Authenticatable>);

    /// Whether a user instance has already been resolved for this request,
    /// without triggering provider resolution. Mirrors Laravel's `hasUser()`:
    /// `true` after a `login`/`once`/`set_user` or a prior `user()` lookup,
    /// `false` when only a session id is present but no user has been fetched.
    async fn has_user(&self) -> bool;

    /// Whether a user is currently authenticated. Defaults to `id().is_some()`.
    async fn check(&self) -> Result<bool, FrameworkError> {
        Ok(self.id().await?.is_some())
    }

    /// Whether the current user is a guest. Defaults to `!check()`.
    async fn guest(&self) -> Result<bool, FrameworkError> {
        Ok(!self.check().await?)
    }
}

/// A guard that can persist authentication across requests (login/logout).
///
/// Session-style guards implement this; stateless token guards implement only
/// [`Guard`].
///
/// The fallible methods that "find a user and authenticate them" return
/// `Result<Option<Arc<dyn Authenticatable>>, _>`: `Ok(Some(user))` on success,
/// `Ok(None)` when no user matched (bad credentials or unknown id), and `Err`
/// only for an underlying failure (database, hashing). This is the Rust-native
/// shape of Laravel's `Authenticatable|false` return — the caller gets the
/// resolved user, and the id is one field away via
/// [`Authenticatable::get_auth_identifier`].
#[async_trait]
pub trait StatefulGuard: Guard {
    /// Validate credentials and, on success, log the user in (persisting to the
    /// session, optionally issuing a remember-me token). Returns the
    /// authenticated user. Mirrors Laravel's `attempt($credentials, $remember)`.
    async fn attempt(
        &self,
        credentials: &Credentials,
        remember: bool,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError>;

    /// Validate credentials and authenticate for the CURRENT request only
    /// (no session persistence). Mirrors Laravel's `once($credentials)`.
    async fn once(&self, credentials: &Credentials) -> Result<bool, FrameworkError>;

    /// Log a known user in (persist to the session, optionally issuing a
    /// remember-me token). Mirrors Laravel's `login($user, $remember)`.
    async fn login(
        &self,
        user: Arc<dyn Authenticatable>,
        remember: bool,
    ) -> Result<(), FrameworkError>;

    /// Log a user in by their identifier, optionally issuing a remember-me
    /// token. Returns the user, or `None` if the provider has no such id.
    /// Mirrors Laravel's `loginUsingId($id, $remember)`.
    async fn login_using_id(
        &self,
        id: &str,
        remember: bool,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError>;

    /// Authenticate by id for the current request only (no session
    /// persistence). Returns the user, or `None` if the provider has no such
    /// id. Mirrors Laravel's `onceUsingId($id)`.
    async fn once_using_id(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError>;

    /// Whether the current user was authenticated via a remember-me cookie
    /// (rather than an active session) this request. Mirrors `viaRemember()`.
    fn via_remember(&self) -> bool;

    /// Log the current user out (clears the session + revokes remember-me).
    async fn logout(&self) -> Result<(), FrameworkError>;
}
