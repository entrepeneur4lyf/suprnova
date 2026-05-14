//! Raw AES-256-GCM encrypt/decrypt.
//!
//! Wire format: `[nonce: 12 bytes][ciphertext || 16-byte GCM tag]`.
//! Empty AAD. A new 12-byte nonce is sampled from `OsRng` per call,
//! giving ~2^48 safe encryptions under a single key.

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm,
};

use super::key::EncryptionKey;
use crate::FrameworkError;

const NONCE_LEN: usize = 12;

/// Encrypt `plaintext` under `key`. Returns the on-wire bytes:
/// `nonce || ciphertext_with_tag`.
pub(crate) fn encrypt(
    key: &EncryptionKey,
    plaintext: &[u8],
) -> Result<Vec<u8>, FrameworkError> {
    let cipher = Aes256Gcm::new(key.as_bytes().into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| FrameworkError::internal(format!("AEAD encrypt failed: {e}")))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(nonce.as_slice());
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt on-wire bytes (`nonce || ciphertext_with_tag`) under `key`.
/// Any tampering — flipped bit, wrong key, truncated nonce — yields an
/// error.
pub(crate) fn decrypt(
    key: &EncryptionKey,
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
        .decrypt(nonce, ciphertext)
        .map_err(|e| FrameworkError::internal(format!("AEAD decrypt failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let key = EncryptionKey::generate();
        let plaintext = b"hello, suprnova";
        let wire = encrypt(&key, plaintext).unwrap();
        let decoded = decrypt(&key, &wire).unwrap();
        assert_eq!(decoded.as_slice(), plaintext);
    }

    #[test]
    fn unique_ciphertexts() {
        let key = EncryptionKey::generate();
        let plaintext = b"same plaintext";
        let a = encrypt(&key, plaintext).unwrap();
        let b = encrypt(&key, plaintext).unwrap();
        // Different nonces → different on-wire output
        assert_ne!(a, b);
        // Both still decrypt back to plaintext
        assert_eq!(decrypt(&key, &a).unwrap(), plaintext);
        assert_eq!(decrypt(&key, &b).unwrap(), plaintext);
    }

    #[test]
    fn tamper_fails() {
        let key = EncryptionKey::generate();
        let mut wire = encrypt(&key, b"don't touch me").unwrap();
        // Flip one bit in the ciphertext body (past the 12-byte nonce)
        let idx = wire.len() - 1;
        wire[idx] ^= 0x01;
        assert!(decrypt(&key, &wire).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let k1 = EncryptionKey::generate();
        let k2 = EncryptionKey::generate();
        let wire = encrypt(&k1, b"secret").unwrap();
        assert!(decrypt(&k2, &wire).is_err());
    }
}
