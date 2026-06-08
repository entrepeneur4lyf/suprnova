//! Authenticatable trait for models that can be authenticated
//!
//! Implement this trait on your User model to enable `Auth::user()`.

use std::any::Any;

/// Trait for models that can be authenticated
///
/// Implement this trait on your User model to enable `Auth::user()`.
///
/// # Identifier model
///
/// Suprnova carries the authenticated user's id as a `String` end-to-end —
/// session storage, [`UserProvider::retrieve_by_id`](super::provider::UserProvider::retrieve_by_id),
/// remember-me, the auth events. Implementors expose that string via
/// [`get_auth_identifier`](Self::get_auth_identifier), which is the canonical
/// id surface (Laravel's `getAuthIdentifier`).
///
/// Numeric ids stringify trivially (`self.id.to_string()`). Opaque ids
/// (UUIDs, ULIDs, torii `UserId`s, external-provider ids) flow through
/// unchanged.
///
/// # Example — numeric primary key
///
/// ```rust,ignore
/// use suprnova::auth::Authenticatable;
/// use std::any::Any;
///
/// impl Authenticatable for User {
///     fn get_auth_identifier(&self) -> String {
///         self.id.to_string()
///     }
///
///     fn as_any(&self) -> &dyn Any {
///         self
///     }
/// }
/// ```
///
/// # Example — opaque string id (UUID, torii)
///
/// ```rust,ignore
/// impl Authenticatable for User {
///     fn get_auth_identifier(&self) -> String {
///         self.id.clone()
///     }
///
///     fn as_any(&self) -> &dyn Any {
///         self
///     }
/// }
/// ```
pub trait Authenticatable: Send + Sync + 'static {
    /// The user's unique identifier as a string. Laravel's `getAuthIdentifier`.
    ///
    /// This is the canonical id surface and the value persisted in the
    /// session + passed to
    /// [`UserProvider::retrieve_by_id`](super::provider::UserProvider::retrieve_by_id).
    ///
    /// Numeric primary keys stringify trivially; UUIDs / ULIDs / opaque
    /// external-provider ids flow through unchanged.
    fn get_auth_identifier(&self) -> String;

    /// Get the name of the identifier column (e.g., "id")
    ///
    /// Override this if your user model uses a different column name.
    fn auth_identifier_name(&self) -> &'static str {
        "id"
    }

    /// The identifier as an `i64`. Convenience for apps whose user id IS a
    /// signed integer primary key — Suprnova itself never calls this; it
    /// works exclusively in terms of [`get_auth_identifier`](Self::get_auth_identifier).
    ///
    /// Defaults to parsing [`get_auth_identifier`](Self::get_auth_identifier),
    /// falling back to `0` for non-numeric ids (UUIDs, opaque tokens). Override
    /// for free when your model already holds an `i64` field — the default
    /// otherwise allocates a `String` just to parse it.
    fn auth_identifier(&self) -> i64 {
        self.get_auth_identifier().parse().unwrap_or(0)
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

    /// Allow downcasting an `Arc<dyn Authenticatable>` to `Arc<T>`
    /// without cloning the concrete user value.
    ///
    /// Every impl writes the same one-line body:
    ///
    /// ```rust,ignore
    /// fn into_arc_any(self: std::sync::Arc<Self>)
    ///     -> std::sync::Arc<dyn std::any::Any + Send + Sync>
    /// {
    ///     self
    /// }
    /// ```
    ///
    /// The boilerplate is unavoidable: a default body would need
    /// `Self: Sized` for the `Arc<Self> → Arc<dyn Any>` coercion, but
    /// adding that bound makes the method non-dyn-safe, which would
    /// break every `Arc<dyn Authenticatable>` site in the framework.
    /// The trait keeps `into_arc_any` dyn-safe by requiring each impl
    /// to supply the body in its own (sized) `impl` block.
    ///
    /// [`Auth::user_as_arc::<T>`](crate::Auth::user_as_arc) calls
    /// this and then `Arc::downcast::<T>` on the returned handle, so
    /// the user value never moves through a clone.
    fn into_arc_any(self: std::sync::Arc<Self>) -> std::sync::Arc<dyn Any + Send + Sync>;
}
