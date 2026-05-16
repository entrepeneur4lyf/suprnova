//! User provider trait for retrieving authenticated users from storage
//!
//! The application must implement this trait and register it with the container
//! to enable `Auth::user()`.

use async_trait::async_trait;
use std::sync::Arc;

use super::authenticatable::Authenticatable;
use crate::error::FrameworkError;

/// Trait for retrieving authenticated users from storage
///
/// The application must implement this trait and register it with the container
/// to enable `Auth::user()`.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::auth::{UserProvider, Authenticatable};
/// use suprnova::FrameworkError;
/// use async_trait::async_trait;
/// use std::sync::Arc;
///
/// pub struct DatabaseUserProvider;
///
/// #[async_trait]
/// impl UserProvider for DatabaseUserProvider {
///     async fn retrieve_by_id(&self, id: &str) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
///         let id: i64 = id.parse().map_err(|_| FrameworkError::bad_request("user id must be numeric"))?;
///         let user = User::query()
///             .filter(Column::Id.eq(id as i32))
///             .first()
///             .await?;
///         Ok(user.map(|u| Arc::new(u) as Arc<dyn Authenticatable>))
///     }
/// }
/// ```
#[async_trait]
pub trait UserProvider: Send + Sync + 'static {
    /// Retrieve a user by their unique identifier
    ///
    /// The `id` is the string stored in the session's `user_id` field.
    /// For apps with numeric primary keys, parse the string: `id.parse::<i64>()`.
    /// For torii-backed apps, this is the raw torii `UserId` string (e.g. `"usr_<base58>"`).
    async fn retrieve_by_id(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError>;

    /// Retrieve a user by credentials (for custom authentication flows)
    ///
    /// Default implementation returns None (not supported).
    /// Override this if you need to authenticate by credentials other than ID.
    async fn retrieve_by_credentials(
        &self,
        _credentials: &serde_json::Value,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        Ok(None)
    }

    /// Validate credentials against a user
    ///
    /// Default implementation returns false (not supported).
    /// Override this if you need password validation.
    async fn validate_credentials(
        &self,
        _user: &dyn Authenticatable,
        _credentials: &serde_json::Value,
    ) -> Result<bool, FrameworkError> {
        Ok(false)
    }
}
