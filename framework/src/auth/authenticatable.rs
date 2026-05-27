//! Authenticatable trait for models that can be authenticated
//!
//! Implement this trait on your User model to enable `Auth::user()`.

use std::any::Any;

/// Trait for models that can be authenticated
///
/// Implement this trait on your User model to enable `Auth::user()`.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::auth::Authenticatable;
/// use std::any::Any;
///
/// impl Authenticatable for User {
///     fn auth_identifier(&self) -> i64 {
///         self.id as i64
///     }
///
///     fn as_any(&self) -> &dyn Any {
///         self
///     }
/// }
/// ```
pub trait Authenticatable: Send + Sync + 'static {
    /// Get the unique identifier for the user (typically the primary key)
    fn auth_identifier(&self) -> i64;

    /// Get the name of the identifier column (e.g., "id")
    ///
    /// Override this if your user model uses a different column name.
    fn auth_identifier_name(&self) -> &'static str {
        "id"
    }

    /// The identifier as a string — the value stored in the session and used
    /// as the guard key. Laravel's `getAuthIdentifier`.
    ///
    /// Defaults to the stringified [`auth_identifier`](Self::auth_identifier).
    /// Override when the user's key isn't a plain integer (e.g. a torii
    /// `UserId` string or a UUID).
    fn get_auth_identifier(&self) -> String {
        self.auth_identifier().to_string()
    }

    /// The hashed password used for credential validation, or `None` if this
    /// user authenticates by other means (OAuth, passkey, magic link).
    /// Laravel's `getAuthPassword`.
    ///
    /// Returning `Some` lets the built-in user providers verify a password via
    /// [`crate::hashing`] without the app implementing a custom provider.
    fn get_auth_password(&self) -> Option<&str> {
        None
    }

    /// Allow downcasting to concrete type
    ///
    /// This is used by `Auth::user_as::<T>()` to cast the trait object
    /// back to the concrete User type.
    fn as_any(&self) -> &dyn Any;
}
