//! Password-based authentication facade.
//!
//! Obtained via [`crate::Auth::password()`].

use super::instance;
use super::{Session, User};
use crate::error::FrameworkError;

/// Facade for password-based authentication operations.
///
/// Delegates to the global Torii instance. Obtain it via [`crate::Auth::password()`].
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::Auth;
///
/// let user = Auth::password()
///     .register("alice@example.com", "s3cret!")
///     .await?;
///
/// let (user, session) = Auth::password()
///     .authenticate("alice@example.com", "s3cret!", None, None)
///     .await?;
/// ```
pub struct PasswordAuth;

impl PasswordAuth {
    /// Register a new user with an email and password.
    ///
    /// If the email already exists the call is idempotent — the existing user is
    /// returned and the password is **not** updated. This prevents user-enumeration
    /// attacks (mirrors Torii's design).
    ///
    /// # Errors
    ///
    /// Returns a [`FrameworkError`] on storage failure or if Torii is not initialised.
    pub async fn register(&self, email: &str, password: &str) -> Result<User, FrameworkError> {
        let torii = instance()?;
        torii
            .password()
            .register(email, password)
            .await
            .map_err(|e| FrameworkError::internal(format!("torii password register: {e}")))
    }

    /// Authenticate a user by email and password, returning the user and a new session.
    ///
    /// `user_agent` and `ip_address` are attached to the session record for auditing.
    ///
    /// # Errors
    ///
    /// Returns a [`FrameworkError`] on invalid credentials, a locked account, or if
    /// Torii is not initialised.
    pub async fn authenticate(
        &self,
        email: &str,
        password: &str,
        user_agent: Option<String>,
        ip_address: Option<String>,
    ) -> Result<(User, Session), FrameworkError> {
        let torii = instance()?;
        torii
            .password()
            .authenticate(email, password, user_agent, ip_address)
            .await
            .map_err(|e| FrameworkError::internal(format!("torii password authenticate: {e}")))
    }
}
