//! Password hashing for suprnova framework
//!
//! Provides secure password hashing using bcrypt, the same default as Laravel.
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::hashing;
//!
//! // Hash a password
//! let hash = hashing::hash("my_password")?;
//!
//! // Verify a password
//! let valid = hashing::verify("my_password", &hash)?;
//! assert!(valid);
//! ```

use crate::error::FrameworkError;

/// Default bcrypt cost factor (same as Laravel)
pub const DEFAULT_COST: u32 = 12;

/// Hash a password using bcrypt with the default cost factor
///
/// # Example
///
/// ```rust,ignore
/// let hash = suprnova::hashing::hash("my_password")?;
/// ```
pub fn hash(password: &str) -> Result<String, FrameworkError> {
    hash_with_cost(password, DEFAULT_COST)
}

/// Hash a password using bcrypt with a custom cost factor
///
/// Higher cost = more secure but slower. Default is 12.
///
/// # Example
///
/// ```rust,ignore
/// let hash = suprnova::hashing::hash_with_cost("my_password", 14)?;
/// ```
pub fn hash_with_cost(password: &str, cost: u32) -> Result<String, FrameworkError> {
    bcrypt::hash(password, cost)
        .map_err(|e| FrameworkError::internal(format!("Password hash error: {}", e)))
}

/// Verify a password against a bcrypt hash
///
/// Uses constant-time comparison to prevent timing attacks.
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
    bcrypt::verify(password, hash)
        .map_err(|e| FrameworkError::internal(format!("Password verify error: {}", e)))
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
