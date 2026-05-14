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

    /// Decode a cursor produced by [`Self::encode_cursor`]. First
    /// attempts encrypted decode via `Crypt::decrypt_string`; on
    /// failure falls back to plain base64 (for backward compatibility
    /// with cursors emitted under an uninitialized Crypt).
    pub fn decode_cursor(wire: &str) -> Result<String, FrameworkError> {
        if let Ok(plain) = Crypt::decrypt_string(wire) {
            return Ok(plain);
        }
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
    fn cursor_decode_handles_legacy_plain_base64() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        // A cursor emitted under a different (or no) key: plain base64
        // of "legacy-value"
        let legacy = URL_SAFE_NO_PAD.encode(b"legacy-value");
        let decoded = CursorPaginator::<i32>::decode_cursor(&legacy).unwrap();
        assert_eq!(decoded, "legacy-value");
    }

    #[test]
    fn cursor_decode_rejects_garbage() {
        let _g = CURSOR_LOCK.lock().unwrap();
        ensure_key();
        // Not valid base64, not valid Crypt ciphertext
        assert!(CursorPaginator::<i32>::decode_cursor("!!! not base64 !!!").is_err());
    }
}
