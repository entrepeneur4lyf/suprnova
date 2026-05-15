//! OAuth authentication facade stub.
//!
//! Full implementation is deferred to Task 6 (P3T6).
//! Methods will be added once the OAuth provider configuration is defined.

/// Facade for OAuth-based authentication operations.
///
/// Obtained via [`crate::Auth::oauth()`].
///
/// Note: No methods are available yet — this type exists so the `Auth` facade can
/// reference it without compilation errors. OAuth support lands in P3T6.
pub struct OAuthAuth;
