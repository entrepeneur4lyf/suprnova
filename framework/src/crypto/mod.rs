//! Application-level encryption.
//!
//! [`Crypt`] is a Laravel-style static facade for AES-256-GCM encryption.
//! The active key is held in a process-wide [`OnceLock`] populated by
//! `Server::from_config()` from the `APP_KEY` environment variable.

pub mod key;
pub(crate) mod aead;

pub use key::EncryptionKey;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{de::DeserializeOwned, Serialize};
use std::sync::OnceLock;

use crate::FrameworkError;

static CRYPT_KEY: OnceLock<EncryptionKey> = OnceLock::new();

/// Process-wide encryption facade.
///
/// Initialize once via [`Crypt::init`] (the framework does this on boot
/// from `APP_KEY`); use the static methods anywhere afterwards.
///
/// # Wire format
///
/// `encrypt_string` and `encrypt` return URL-safe base64 (no padding)
/// over `nonce || ciphertext_with_tag`. Empty AAD. Each call gets a
/// fresh random nonce.
pub struct Crypt;

impl Crypt {
    /// Install the process-wide encryption key.
    ///
    /// Subsequent calls are a no-op and emit a `tracing::warn!` — the
    /// key is sealed for the lifetime of the process.
    pub fn init(key: EncryptionKey) {
        if CRYPT_KEY.set(key).is_err() {
            tracing::warn!("Crypt::init called more than once; ignoring");
        }
    }

    /// Whether a key has been installed.
    pub fn is_initialized() -> bool {
        CRYPT_KEY.get().is_some()
    }

    fn key() -> Result<&'static EncryptionKey, FrameworkError> {
        CRYPT_KEY.get().ok_or_else(|| {
            FrameworkError::internal(
                "Crypt is not initialized — set APP_KEY before serving",
            )
        })
    }

    /// Encrypt a UTF-8 string. Returns base64-url-no-pad over
    /// `nonce || ciphertext_with_tag`.
    pub fn encrypt_string(plaintext: &str) -> Result<String, FrameworkError> {
        let key = Self::key()?;
        let wire = aead::encrypt(key, plaintext.as_bytes())?;
        Ok(URL_SAFE_NO_PAD.encode(wire))
    }

    /// Decrypt a base64-url-no-pad payload previously produced by
    /// [`Self::encrypt_string`].
    pub fn decrypt_string(wire: &str) -> Result<String, FrameworkError> {
        let key = Self::key()?;
        let bytes = URL_SAFE_NO_PAD
            .decode(wire.trim())
            .map_err(|e| FrameworkError::internal(format!("Crypt base64 decode failed: {e}")))?;
        let plain = aead::decrypt(key, &bytes)?;
        String::from_utf8(plain)
            .map_err(|e| FrameworkError::internal(format!("Crypt decrypted bytes not UTF-8: {e}")))
    }

    /// Encrypt any `Serialize` value by JSON-encoding then encrypting.
    pub fn encrypt<T: Serialize>(value: &T) -> Result<String, FrameworkError> {
        let key = Self::key()?;
        let json = serde_json::to_vec(value)
            .map_err(|e| FrameworkError::internal(format!("Crypt JSON encode failed: {e}")))?;
        let wire = aead::encrypt(key, &json)?;
        Ok(URL_SAFE_NO_PAD.encode(wire))
    }

    /// Decrypt and JSON-decode a payload previously produced by
    /// [`Self::encrypt`].
    pub fn decrypt<T: DeserializeOwned>(wire: &str) -> Result<T, FrameworkError> {
        let key = Self::key()?;
        let bytes = URL_SAFE_NO_PAD
            .decode(wire.trim())
            .map_err(|e| FrameworkError::internal(format!("Crypt base64 decode failed: {e}")))?;
        let plain = aead::decrypt(key, &bytes)?;
        serde_json::from_slice(&plain)
            .map_err(|e| FrameworkError::internal(format!("Crypt JSON decode failed: {e}")))
    }
}

#[doc(hidden)]
/// Test-only helper: install a key without going through `OnceLock::set`
/// for the second-and-later test in a suite. Returns `true` if the
/// key was actually installed, `false` if a key was already present.
///
/// Tests must serialize themselves via a `Mutex<()>` because the global
/// `CRYPT_KEY` is shared.
///
/// **Test-only — do not call from production code.** The double-leading-
/// underscore name signals "internal test fixture, not API." Marked
/// `#[doc(hidden)]` to keep it out of public rustdoc. A proper
/// `testing` cargo feature flag would gate this; until that flag is
/// added (see roadmap), the leading underscores + doc(hidden) are
/// the available signal.
#[doc(hidden)]
pub fn _test_install_key(key: EncryptionKey) -> bool {
    CRYPT_KEY.set(key).is_ok()
}
