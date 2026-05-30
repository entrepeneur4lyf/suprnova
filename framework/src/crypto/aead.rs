//! Raw AES-256-GCM encrypt/decrypt.
//!
//! Wire format: `[nonce: 12 bytes][ciphertext || 16-byte GCM tag]`.
//! A new 12-byte nonce is sampled from `OsRng` per call, giving ~2^48
//! safe encryptions under a single key.
//!
//! # Associated Data (AAD)
//!
//! Both `encrypt` and `decrypt` take an `aad: &[u8]` byte string that is
//! authenticated alongside the ciphertext but not encrypted into it.
//! GCM mixes the AAD into the authentication tag so any tampering with
//! the AAD on either side fails the tag check.
//!
//! Suprnova uses the AAD to bind ciphertext to a *purpose* (cookie,
//! cursor, 2FA secret, etc.) — see [`super::CryptPurpose`]. Reusing the
//! same key under different AADs gives cryptographic domain separation:
//! ciphertext produced for one surface fails to decrypt on another. The
//! AAD itself does NOT travel on the wire — both encrypt and decrypt
//! supply the same bytes by independent agreement.

use aes_gcm::{
    Aes256Gcm,
    aead::{Aead, AeadCore, KeyInit, OsRng, Payload},
};

use super::key::EncryptionKey;
use crate::FrameworkError;

const NONCE_LEN: usize = 12;

/// Encrypt `plaintext` under `key` with `aad` bound into the GCM
/// authentication tag. Returns the on-wire bytes:
/// `nonce || ciphertext_with_tag`.
///
/// `aad` is NOT serialised into the output — only its contribution to
/// the authentication tag survives. Decrypt callers must supply the
/// byte-identical `aad` to verify.
pub(crate) fn encrypt(
    key: &EncryptionKey,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, FrameworkError> {
    let cipher = Aes256Gcm::new(key.as_bytes().into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|e| FrameworkError::internal(format!("AEAD encrypt failed: {e}")))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt on-wire bytes (`nonce || ciphertext_with_tag`) under `key`
/// with `aad` bound into the GCM authentication tag. Any tampering —
/// flipped bit, wrong key, truncated nonce, *mismatched AAD* — yields
/// an error.
///
/// A wire produced by `encrypt(key, aad_a, ...)` will fail `decrypt`
/// with any `aad_b != aad_a`. This is the domain-separation guarantee.
pub(crate) fn decrypt(
    key: &EncryptionKey,
    aad: &[u8],
    wire: &[u8],
) -> Result<Vec<u8>, FrameworkError> {
    if wire.len() < NONCE_LEN + 16 {
        return Err(FrameworkError::internal(
            "AEAD wire too short (need nonce + 16-byte tag)",
        ));
    }
    let (nonce_bytes, ciphertext) = wire.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(key.as_bytes().into());
    let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|e| FrameworkError::internal(format!("AEAD decrypt failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AAD: &[u8] = b"suprnova:test:v1";

    #[test]
    fn round_trip() {
        let key = EncryptionKey::generate();
        let plaintext = b"hello, suprnova";
        let wire = encrypt(&key, AAD, plaintext).unwrap();
        let decoded = decrypt(&key, AAD, &wire).unwrap();
        assert_eq!(decoded.as_slice(), plaintext);
    }

    #[test]
    fn unique_ciphertexts() {
        let key = EncryptionKey::generate();
        let plaintext = b"same plaintext";
        let a = encrypt(&key, AAD, plaintext).unwrap();
        let b = encrypt(&key, AAD, plaintext).unwrap();
        // Different nonces → different on-wire output
        assert_ne!(a, b);
        // Both still decrypt back to plaintext
        assert_eq!(decrypt(&key, AAD, &a).unwrap(), plaintext);
        assert_eq!(decrypt(&key, AAD, &b).unwrap(), plaintext);
    }

    #[test]
    fn tamper_fails() {
        let key = EncryptionKey::generate();
        let mut wire = encrypt(&key, AAD, b"don't touch me").unwrap();
        // Flip one bit in the ciphertext body (past the 12-byte nonce)
        let idx = wire.len() - 1;
        wire[idx] ^= 0x01;
        assert!(decrypt(&key, AAD, &wire).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let k1 = EncryptionKey::generate();
        let k2 = EncryptionKey::generate();
        let wire = encrypt(&k1, AAD, b"secret").unwrap();
        assert!(decrypt(&k2, AAD, &wire).is_err());
    }

    #[test]
    fn mismatched_aad_fails() {
        // The domain-separation guarantee: a wire produced under one AAD
        // is rejected by decrypt under a different AAD even when key,
        // nonce, and ciphertext bytes are otherwise identical. This is
        // the property the framework relies on to prevent cross-surface
        // ciphertext replay.
        let key = EncryptionKey::generate();
        let wire = encrypt(&key, b"suprnova:cookie:v1", b"session-id").unwrap();
        assert!(decrypt(&key, b"suprnova:cursor:v1", &wire).is_err());
        // Sanity: same AAD still decrypts.
        let plain = decrypt(&key, b"suprnova:cookie:v1", &wire).unwrap();
        assert_eq!(plain, b"session-id");
    }

    #[test]
    fn empty_aad_is_distinct_purpose() {
        // Empty AAD is a valid (degenerate) purpose. A wire produced
        // with empty AAD must NOT decrypt under a non-empty AAD and
        // vice versa — there's no implicit "any AAD" mode.
        let key = EncryptionKey::generate();
        let with_aad = encrypt(&key, AAD, b"payload").unwrap();
        assert!(decrypt(&key, b"", &with_aad).is_err());
        let without_aad = encrypt(&key, b"", b"payload").unwrap();
        assert!(decrypt(&key, AAD, &without_aad).is_err());
    }
}
