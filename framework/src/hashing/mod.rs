//! Password hashing for suprnova framework
//!
//! Provides secure password hashing using bcrypt, the same default as Laravel.
//!
//! # Two flavours — async-safe and sync
//!
//! Bcrypt at cost 12 is intentionally CPU-bound: a single hash takes
//! ~250ms on modern hardware. Calling `hash` / `verify` directly from
//! a Tokio request handler blocks the worker thread for the whole
//! duration. Use the `*_async` variants ([`hash_async`],
//! [`hash_with_cost_async`], [`verify_async`]) inside `async fn`
//! handlers — they wrap the bcrypt call in `tokio::task::spawn_blocking`
//! so the worker stays free for other requests. The sync variants
//! stay for tests, CLI tools, and other non-async call sites.
//!
//! # 72-byte password ceiling
//!
//! Bcrypt's internal block size limits passwords to 72 bytes — the
//! crate's `hash` / `verify` functions silently truncate longer
//! inputs, which means two distinct passphrases that share their
//! first 72 bytes hash to the same value (audit HIGH `hashing` #2).
//! This module's `hash` / `verify` reject passwords > 72 bytes
//! up-front:
//!
//! - `hash` returns `FrameworkError::param("password exceeds 72 bytes")`.
//! - `verify` returns `Ok(false)` so the calling auth flow surfaces
//!   the same "invalid credentials" response regardless — no
//!   length-based information disclosure.
//!
//! The underlying bcrypt call uses the `non_truncating_*` variants
//! as defense in depth.
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::hashing;
//!
//! // Async (preferred inside request handlers):
//! let hash = hashing::hash_async("my_password").await?;
//! let valid = hashing::verify_async("my_password", &hash).await?;
//!
//! // Sync (tests, CLI tools, non-async contexts):
//! let hash = hashing::hash("my_password")?;
//! let valid = hashing::verify("my_password", &hash)?;
//! ```

use crate::error::FrameworkError;

/// Default bcrypt cost factor (same as Laravel)
pub const DEFAULT_COST: u32 = 12;

/// Maximum password length accepted by [`hash`] / [`verify`] (and
/// their `_async` siblings). Bcrypt requires a trailing null byte
/// inside its 72-byte block, so the usable password limit is 71
/// bytes — `non_truncating_hash` itself errors with
/// `"Expected 72 bytes or fewer; found 73 bytes"` when handed
/// exactly 72 password bytes. The framework rejects up-front to
/// prevent two distinct passphrases with the same first 71 bytes
/// from authenticating as the same password. See module docs.
pub const MAX_PASSWORD_BYTES: usize = 71;

/// Hash a password using bcrypt with the default cost factor.
///
/// **Synchronous** — blocks the calling thread for ~250ms at cost 12.
/// Use [`hash_async`] inside Tokio request handlers.
///
/// Returns `FrameworkError::param` if `password` exceeds
/// [`MAX_PASSWORD_BYTES`] (72 bytes) — see module docs for the
/// rationale.
///
/// # Example
///
/// ```rust,ignore
/// let hash = suprnova::hashing::hash("my_password")?;
/// ```
pub fn hash(password: &str) -> Result<String, FrameworkError> {
    hash_with_cost(password, DEFAULT_COST)
}

/// Hash a password using bcrypt with a custom cost factor.
///
/// **Synchronous** — see [`hash_with_cost_async`] for the async-safe
/// variant. Higher cost = more secure but slower; default is 12.
/// Rejects passwords > [`MAX_PASSWORD_BYTES`].
///
/// # Example
///
/// ```rust,ignore
/// let hash = suprnova::hashing::hash_with_cost("my_password", 14)?;
/// ```
pub fn hash_with_cost(password: &str, cost: u32) -> Result<String, FrameworkError> {
    enforce_password_length(password)?;
    bcrypt::non_truncating_hash(password, cost)
        .map_err(|e| FrameworkError::internal(format!("Password hash error: {}", e)))
}

/// Verify a password against a bcrypt hash.
///
/// **Synchronous** — see [`verify_async`] for the async-safe variant.
/// Uses constant-time comparison to prevent timing attacks. Passwords
/// longer than [`MAX_PASSWORD_BYTES`] cannot match any hash this module
/// produces, so they return `Ok(false)` rather than an error — keeps
/// the calling auth flow returning the same "invalid credentials"
/// response regardless of length.
///
/// # Example
///
/// ```rust,ignore
/// let valid = suprnova::hashing::verify("my_password", &stored_hash)?;
/// if valid {
///     // Password is correct
/// }
/// ```
pub fn verify(password: &str, hash: &str) -> Result<bool, FrameworkError> {
    if password.len() > MAX_PASSWORD_BYTES {
        return Ok(false);
    }
    bcrypt::verify(password, hash)
        .map_err(|e| FrameworkError::internal(format!("Password verify error: {}", e)))
}

/// Async-safe wrapper around [`hash`] — runs the CPU-bound bcrypt
/// work on `tokio::task::spawn_blocking` so the calling worker
/// thread stays free for other requests.
///
/// # Example
///
/// ```rust,ignore
/// let hash = suprnova::hashing::hash_async("my_password").await?;
/// ```
pub async fn hash_async(password: &str) -> Result<String, FrameworkError> {
    hash_with_cost_async(password, DEFAULT_COST).await
}

/// Async-safe wrapper around [`hash_with_cost`].
pub async fn hash_with_cost_async(password: &str, cost: u32) -> Result<String, FrameworkError> {
    let owned = password.to_string();
    tokio::task::spawn_blocking(move || hash_with_cost(&owned, cost))
        .await
        .map_err(|e| FrameworkError::internal(format!("hash_async join error: {e}")))?
}

/// Async-safe wrapper around [`verify`].
///
/// # Example
///
/// ```rust,ignore
/// let valid = suprnova::hashing::verify_async("my_password", &stored).await?;
/// ```
pub async fn verify_async(password: &str, hash: &str) -> Result<bool, FrameworkError> {
    let pw = password.to_string();
    let h = hash.to_string();
    tokio::task::spawn_blocking(move || verify(&pw, &h))
        .await
        .map_err(|e| FrameworkError::internal(format!("verify_async join error: {e}")))?
}

fn enforce_password_length(password: &str) -> Result<(), FrameworkError> {
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(FrameworkError::param(format!(
            "password exceeds {MAX_PASSWORD_BYTES}-byte bcrypt usable limit (block size 72 minus null terminator) (got {} bytes); \
             reject at the form-input layer or split into a longer-key derivation",
            password.len()
        )));
    }
    Ok(())
}

/// Check if a hash needs to be rehashed (e.g., if cost factor changed)
///
/// # Example
///
/// ```rust,ignore
/// if suprnova::hashing::needs_rehash(&stored_hash) {
///     let new_hash = suprnova::hashing::hash("password")?;
///     // Store new_hash
/// }
/// ```
pub fn needs_rehash(hash: &str) -> bool {
    // Parse the bcrypt hash to get its cost
    // Format: $2a$XX$... or $2b$XX$... where XX is the cost
    let parts: Vec<&str> = hash.split('$').collect();
    if parts.len() < 4 {
        return true; // Invalid hash format
    }

    let cost: u32 = parts[2].parse().unwrap_or(0);
    cost < DEFAULT_COST
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_and_verify() {
        let password = "test_password_123";
        let hashed = hash(password).expect("Hash should succeed");

        // Hash should be a valid bcrypt hash
        assert!(hashed.starts_with("$2"));

        // Verification should succeed with correct password
        assert!(verify(password, &hashed).expect("Verify should succeed"));

        // Verification should fail with wrong password
        assert!(!verify("wrong_password", &hashed).expect("Verify should succeed"));
    }

    #[test]
    fn test_hash_with_custom_cost() {
        let password = "test";
        let hashed = hash_with_cost(password, 4).expect("Hash should succeed");
        assert!(verify(password, &hashed).expect("Verify should succeed"));
    }

    #[test]
    fn test_needs_rehash() {
        // Low cost hash should need rehash
        let low_cost_hash = hash_with_cost("test", 4).expect("Hash should succeed");
        assert!(needs_rehash(&low_cost_hash));

        // Default cost hash should not need rehash
        let default_cost_hash = hash("test").expect("Hash should succeed");
        assert!(!needs_rehash(&default_cost_hash));
    }
}
