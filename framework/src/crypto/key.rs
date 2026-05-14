//! 32-byte symmetric encryption key (AES-256).
//!
//! Loaded from the `APP_KEY` environment variable in base64-url-no-pad
//! form, or generated for development.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::{rngs::OsRng, RngCore};

use crate::FrameworkError;

/// A 32-byte symmetric key used for AES-256-GCM.
///
/// The `Debug` impl prints `"[REDACTED]"` so the key never leaks into
/// logs or panic messages.
#[derive(Clone)]
pub struct EncryptionKey([u8; 32]);

impl EncryptionKey {
    /// Load the key from the `APP_KEY` environment variable. The value
    /// must be a 32-byte key encoded as URL-safe base64 with no padding.
    ///
    /// Returns `FrameworkError::Internal` if the variable is unset or the
    /// value cannot be decoded.
    pub fn from_env() -> Result<Self, FrameworkError> {
        let raw = std::env::var("APP_KEY").map_err(|_| {
            FrameworkError::internal(
                "APP_KEY is not set (expected base64 URL-safe, no padding, 32 bytes)",
            )
        })?;
        Self::from_base64(&raw)
    }

    /// Generate a new random 32-byte key using the OS RNG.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// Decode a base64-url-no-pad encoded key. Rejects any input that
    /// does not decode to exactly 32 bytes.
    pub fn from_base64(s: &str) -> Result<Self, FrameworkError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(s.trim())
            .map_err(|e| FrameworkError::internal(format!("APP_KEY base64 decode failed: {e}")))?;
        if bytes.len() != 32 {
            return Err(FrameworkError::internal(format!(
                "APP_KEY must decode to 32 bytes, got {}",
                bytes.len()
            )));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(Self(out))
    }

    /// Encode this key as URL-safe base64 with no padding.
    pub fn to_base64(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.0)
    }

    /// Return the raw 32 bytes — used by the AEAD layer.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("EncryptionKey").field(&"[REDACTED]").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn round_trips_base64() {
        let key = EncryptionKey::generate();
        let encoded = key.to_base64();
        let decoded = EncryptionKey::from_base64(&encoded).expect("round-trip decode");
        assert_eq!(key.as_bytes(), decoded.as_bytes());
        // URL-safe no-pad never contains '+', '/', or '='
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
    }

    #[test]
    fn rejects_wrong_length() {
        // 16 bytes encoded — too short
        let short = URL_SAFE_NO_PAD.encode([0u8; 16]);
        assert!(EncryptionKey::from_base64(&short).is_err());
        // 64 bytes encoded — too long
        let long = URL_SAFE_NO_PAD.encode([0u8; 64]);
        assert!(EncryptionKey::from_base64(&long).is_err());
        // Garbage
        assert!(EncryptionKey::from_base64("not-valid-base64!!!").is_err());
    }

    #[test]
    fn from_env_reads_app_key() {
        let _g = ENV_LOCK.lock().unwrap();
        let key = EncryptionKey::generate();
        let encoded = key.to_base64();
        // SAFETY: ENV_LOCK serializes env access within this module
        unsafe {
            std::env::set_var("APP_KEY", &encoded);
        }
        let loaded = EncryptionKey::from_env().expect("loaded from APP_KEY");
        assert_eq!(loaded.as_bytes(), key.as_bytes());
        // SAFETY: ENV_LOCK serializes env access within this module
        unsafe {
            std::env::remove_var("APP_KEY");
        }
        assert!(EncryptionKey::from_env().is_err());
    }

    #[test]
    fn debug_impl_redacts() {
        let key = EncryptionKey::from_base64(&URL_SAFE_NO_PAD.encode([0xABu8; 32])).unwrap();
        let dbg = format!("{:?}", key);
        assert!(dbg.contains("REDACTED"));
        // The raw bytes must not appear in the Debug output
        assert!(!dbg.contains("ab, ab"));
        assert!(!dbg.contains("171"));
    }
}
