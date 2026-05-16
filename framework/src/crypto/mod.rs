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

use crate::config::Environment;
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

/// Boot-time policy decision: given the runtime environment and the raw
/// value of `APP_KEY` (`None` if unset, `Some("")` if set-but-empty —
/// callers may pass either), decide which [`EncryptionKey`] to install.
///
/// This is the pure function that backs the `Server::from_config` boot
/// path. Production fails closed: missing or empty `APP_KEY` is an
/// `Err` with an actionable message. Local/development/testing fall
/// back to a freshly-generated transient key so dev workflows stay
/// zero-config — but the caller is expected to log a warn so the
/// operator notices.
///
/// Custom environments (`Environment::Custom`) and `Staging` are
/// treated as production-like — they fail closed if no key is
/// supplied. The bar is "if it's not obviously a dev environment,
/// don't downgrade encryption."
///
/// # Errors
///
/// - Production / Staging / Custom env without a valid key
/// - Any environment with a malformed `APP_KEY` (wrong length, bad
///   base64) — bad keys never fall through to a generated dev key
///   because that would silently mask a misconfigured production
///   deployment.
pub fn resolve_boot_key(
    environment: &Environment,
    app_key: Option<&str>,
) -> Result<BootKey, FrameworkError> {
    // Treat empty string the same as unset — both mean "no key
    // configured." Strips trailing whitespace too so a `APP_KEY=` line
    // with a stray space doesn't accidentally parse.
    let supplied = app_key.map(str::trim).filter(|s| !s.is_empty());

    match (environment, supplied) {
        (_, Some(raw)) => {
            // Explicit key always wins. A malformed key is an error in
            // every environment — never fall back to a generated dev
            // key because that would mask a typo in production.
            let key = EncryptionKey::from_base64(raw).map_err(|e| {
                FrameworkError::internal(format!(
                    "APP_KEY is set but invalid: {e}. Expected 32 bytes \
                     encoded as URL-safe base64 (no padding). Run \
                     `suprnova key:generate` to mint a new one."
                ))
            })?;
            Ok(BootKey::Configured(key))
        }
        (Environment::Local | Environment::Development | Environment::Testing, None) => {
            // Dev environments still need a key for sessions and
            // cursors to work — we just don't require the operator
            // to set one up before `cargo run`. Generated transient
            // keys reset on every restart, which is a feature in
            // development (no stale-session weirdness) but the
            // caller should log a warn so the operator knows
            // sessions won't persist across boots.
            Ok(BootKey::GeneratedTransient(EncryptionKey::generate()))
        }
        (env, None) => Err(FrameworkError::internal(format!(
            "APP_KEY is required when APP_ENV={env}. Generate one with \
             `suprnova key:generate` and set it in your environment \
             (e.g. .env or your secrets manager). Suprnova refuses to \
             boot without an encryption key outside of local/development/\
             testing because session cookies and pagination cursors would \
             otherwise be unsigned and forgeable."
        ))),
    }
}

/// Result of [`resolve_boot_key`]. The caller installs the inner key
/// via [`Crypt::init`]; the discriminator is preserved so the boot
/// path can emit the right log message (a generated dev key needs a
/// loud warn that the operator may want to persist it).
#[derive(Debug)]
pub enum BootKey {
    /// Operator supplied a valid `APP_KEY` in the environment.
    Configured(EncryptionKey),
    /// No `APP_KEY` set and the environment permits a transient dev
    /// key. The boot path generated a fresh random key on the spot —
    /// it will not survive a restart.
    GeneratedTransient(EncryptionKey),
}

impl BootKey {
    pub fn into_key(self) -> EncryptionKey {
        match self {
            BootKey::Configured(k) | BootKey::GeneratedTransient(k) => k,
        }
    }

    pub fn is_generated(&self) -> bool {
        matches!(self, BootKey::GeneratedTransient(_))
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

#[cfg(test)]
mod boot_tests {
    //! Tests for [`resolve_boot_key`]. These do NOT touch the global
    //! `CRYPT_KEY` `OnceLock` — they exercise the pure decision
    //! function. End-to-end Crypt installation is covered by
    //! `framework/tests/app_key_enforcement.rs` (one scenario per test
    //! binary because `OnceLock` is process-wide).

    use super::*;

    #[test]
    fn production_without_key_fails_closed() {
        let err = resolve_boot_key(&Environment::Production, None).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("APP_KEY is required"),
            "expected actionable message, got: {msg}"
        );
        assert!(
            msg.contains("suprnova key:generate"),
            "message should point at the CLI helper, got: {msg}"
        );
    }

    #[test]
    fn production_with_empty_key_fails_closed() {
        // Empty string and whitespace-only count as "unset" — the
        // operator likely has `APP_KEY=` with nothing after the equals.
        assert!(resolve_boot_key(&Environment::Production, Some("")).is_err());
        assert!(resolve_boot_key(&Environment::Production, Some("   ")).is_err());
    }

    #[test]
    fn staging_without_key_fails_closed() {
        assert!(resolve_boot_key(&Environment::Staging, None).is_err());
    }

    #[test]
    fn custom_env_without_key_fails_closed() {
        // Unknown environments are treated production-like — anything
        // we don't explicitly recognize as a dev environment must not
        // silently downgrade.
        assert!(
            resolve_boot_key(&Environment::Custom("k8s".into()), None).is_err()
        );
    }

    #[test]
    fn production_with_valid_key_succeeds() {
        let key = EncryptionKey::generate().to_base64();
        let resolved = resolve_boot_key(&Environment::Production, Some(&key)).unwrap();
        assert!(!resolved.is_generated());
    }

    #[test]
    fn production_with_malformed_key_errors_even_with_value() {
        // A bad key in production must error — never fall back to a
        // generated key, because that would mask a typo or a
        // half-rotated secret.
        let err =
            resolve_boot_key(&Environment::Production, Some("not-valid-base64!!!")).unwrap_err();
        assert!(format!("{err}").contains("APP_KEY is set but invalid"));
    }

    #[test]
    fn dev_env_without_key_generates_transient() {
        for env in [
            Environment::Local,
            Environment::Development,
            Environment::Testing,
        ] {
            let resolved = resolve_boot_key(&env, None).unwrap();
            assert!(
                resolved.is_generated(),
                "expected generated transient key for {env}, got Configured"
            );
        }
    }

    #[test]
    fn dev_env_with_explicit_key_uses_it() {
        // Even in local, if the operator supplies a key we use it
        // (sessions persist across restarts).
        let key = EncryptionKey::generate().to_base64();
        let resolved = resolve_boot_key(&Environment::Local, Some(&key)).unwrap();
        assert!(!resolved.is_generated());
    }

    #[test]
    fn dev_env_with_malformed_key_still_errors() {
        // Even in local, an explicit-but-bad key is an error — better
        // to fail at boot than silently mask a typo.
        let err =
            resolve_boot_key(&Environment::Local, Some("not-valid-base64!!!")).unwrap_err();
        assert!(format!("{err}").contains("APP_KEY is set but invalid"));
    }
}
