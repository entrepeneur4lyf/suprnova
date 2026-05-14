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

    /// Allow downcasting to concrete type
    ///
    /// This is used by `Auth::user_as::<T>()` to cast the trait object
    /// back to the concrete User type.
    fn as_any(&self) -> &dyn Any;
}
