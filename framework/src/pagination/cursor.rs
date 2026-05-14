//! Cursor paginator — keyset-style pagination with encrypted cursors.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Serialize;

use crate::crypto::Crypt;
use crate::FrameworkError;

/// Paginator that emits opaque cursor strings instead of page numbers.
///
/// Equivalent to Laravel's `CursorPaginator`. Returned by
/// [`Pagination::cursor`](crate::pagination::Pagination::cursor).
#[derive(Debug, Clone, Serialize)]
pub struct CursorPaginator<T> {
    /// The rows on this page.
    pub data: Vec<T>,
    /// Cursor to fetch the next page, or `None` at the last page.
    pub next_cursor: Option<String>,
    /// Cursor for the previous page. Always `None` in v1
    /// (single-direction cursor). Tracked here so adding a backward
    /// path later does not break the API.
    pub prev_cursor: Option<String>,
}

impl<T> CursorPaginator<T> {
    /// Encode a cursor boundary value. When `Crypt` is initialized,
    /// the value is AES-256-GCM encrypted and base64-url-no-pad
    /// encoded (using the same wire format as `Crypt::encrypt_string`).
    /// When `Crypt` is not initialized, falls back to plain
    /// base64-url-no-pad with a one-time `tracing::warn!`.
    pub fn encode_cursor(value: &str) -> String {
        match Crypt::encrypt_string(value) {
            Ok(wire) => wire,
            Err(_) => {
                tracing::warn!(
                    "Crypt not initialized — cursors will be plain base64. \
                     Set APP_KEY before deploying."
                );
                URL_SAFE_NO_PAD.encode(value.as_bytes())
            }
        }
    }

    /// Decode a cursor produced by [`Self::encode_cursor`].
    ///
    /// **Security:** When `Crypt` is initialized, ONLY the encrypted
    /// path is used. Any decrypt failure (tampering, wrong key,
    /// truncation) is propagated as an error — there is NO fallback
    /// to plain base64, because that would let an attacker bypass the
    /// AEAD integrity check by submitting a base64-encoded boundary
    /// value of their choice.
    ///
    /// When `Crypt` is NOT initialized (deployment without `APP_KEY`),
    /// the plain base64 path is the only path available. Cursors in
    /// that mode are not tamper-resistant; this is documented as a
    /// limitation of running without an encryption key.
    pub fn decode_cursor(wire: &str) -> Result<String, FrameworkError> {
        if Crypt::is_initialized() {
            // With a key installed, only the authenticated path is valid.
            // A failure here means the cursor was tampered, truncated,
            // or generated under a different key — never trust it.
            return Crypt::decrypt_string(wire);
        }
        // No key installed: plain base64 is the only option. Cursors
        // emitted in this mode are not tamper-resistant; deployments
        // that need integrity must set APP_KEY.
        let bytes = URL_SAFE_NO_PAD.decode(wire.trim()).map_err(|e| {
            FrameworkError::internal(format!("Cursor decode failed: {e}"))
        })?;
        String::from_utf8(bytes)
            .map_err(|e| FrameworkError::internal(format!("Cursor not UTF-8: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cursor tests share Crypt state with the encryption suite. We use
    // the same install-once pattern; either suite may install first.
    use std::sync::Mutex;
    static CURSOR_LOCK: Mutex<()> = Mutex::new(());

    fn ensure_key() {
        // _test_install_key returns false if a key is already present —
        // that's fine; we just need *some* key in the OnceLock.
        let _ = crate::crypto::_test_install_key(crate::EncryptionKey::generate());
    }

    #[test]
    fn encrypted_cursor_round_trip() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let wire = CursorPaginator::<i32>::encode_cursor("user-42");
        // With Crypt active, cursor is opaque (not equal to base64 of plaintext)
        let plain_b64 = URL_SAFE_NO_PAD.encode(b"user-42");
        assert_ne!(wire, plain_b64);
        let decoded = CursorPaginator::<i32>::decode_cursor(&wire).unwrap();
        assert_eq!(decoded, "user-42");
    }

    #[test]
    fn cursor_decode_rejects_plain_base64_when_crypt_initialized() {
        // Security regression test: when Crypt has a key, an
        // attacker-crafted plain-base64 cursor MUST be rejected. The
        // fallback exists ONLY for deployments without APP_KEY.
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        let attacker_cursor = URL_SAFE_NO_PAD.encode(b"42"); // any plain int
        // Should fail because Crypt is initialized; only AEAD-verified
        // cursors are accepted.
        assert!(CursorPaginator::<i32>::decode_cursor(&attacker_cursor).is_err());
    }

    #[test]
    fn cursor_decode_rejects_garbage() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        // Not valid base64, not valid Crypt ciphertext
        assert!(CursorPaginator::<i32>::decode_cursor("!!! not base64 !!!").is_err());
    }
}
