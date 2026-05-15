//! Passkey (WebAuthn/FIDO2) authentication facade stub.
//!
//! Full implementation is deferred to Task 7 (P3T7).
//! Methods will be added once the passkey credential flow is defined.

/// Facade for passkey (WebAuthn/FIDO2) authentication operations.
///
/// Obtained via [`crate::Auth::passkey()`].
///
/// Note: No methods are available yet — this type exists so the `Auth` facade can
/// reference it without compilation errors. Passkey support lands in P3T7.
pub struct PasskeyAuth;
