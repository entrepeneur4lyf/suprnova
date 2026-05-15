//! Magic-link authentication facade stub.
//!
//! Full implementation is deferred to Task 5 (P3T5).
//! Methods will be added once the mailer integration is wired up.

/// Facade for magic-link authentication operations.
///
/// Obtained via [`crate::Auth::magic_link()`].
///
/// Note: No methods are available yet — this type exists so the `Auth` facade can
/// reference it without compilation errors. Magic-link support lands in P3T5.
pub struct MagicLinkAuth;
