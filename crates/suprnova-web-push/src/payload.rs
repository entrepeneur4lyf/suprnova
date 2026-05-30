//! Payload encryption per RFC 8291 (Web Push) using AES128GCM.
//!
//! Delegates the actual crypto to the upstream `ece` crate.

use crate::error::WebPushError;
use base64::Engine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentEncoding {
    /// RFC 8188 + RFC 8291 — the current Web Push standard.
    Aes128Gcm,
}

impl ContentEncoding {
    pub fn header_value(&self) -> &'static str {
        match self {
            Self::Aes128Gcm => "aes128gcm",
        }
    }
}

/// Maximum plaintext bytes — leaves headroom for the ~85-byte AES128GCM
/// encryption overhead so the final payload fits the 4096-byte push-service cap.
pub const MAX_PLAINTEXT_BYTES: usize = 3992;

/// Required length of the receiver's P-256 uncompressed public key
/// (`p256dh`) after base64url decoding, per RFC 8291 §3.1: a 65-byte
/// uncompressed SEC1 point (`0x04 || X || Y`, each coordinate 32 bytes).
/// Matches `ECE_WEBPUSH_PUBLIC_KEY_LENGTH` in the upstream `ece` crate.
pub const P256DH_KEY_LEN: usize = 65;

/// Required length of the receiver's auth secret after base64url
/// decoding, per RFC 8291 §3.2: a 16-byte uniformly-random value. Matches
/// `ECE_WEBPUSH_AUTH_SECRET_LENGTH` in the upstream `ece` crate.
pub const AUTH_SECRET_LEN: usize = 16;

/// One encrypted push payload, ready to send.
#[derive(Debug, Clone)]
pub struct Payload {
    body: Vec<u8>,
    content_encoding: ContentEncoding,
}

impl Payload {
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    pub fn content_encoding(&self) -> ContentEncoding {
        self.content_encoding
    }

    /// Encrypt `plaintext` for the given subscriber. `p256dh_b64url` and
    /// `auth_b64url` come from the browser's `PushSubscription.getKey('p256dh')`
    /// and `getKey('auth')` exports (base64url, no padding).
    pub fn encrypt(
        plaintext: &[u8],
        p256dh_b64url: &str,
        auth_b64url: &str,
        encoding: ContentEncoding,
    ) -> Result<Self, WebPushError> {
        if plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(WebPushError::Encryption(format!(
                "payload too large: {} bytes > {} byte cap",
                plaintext.len(),
                MAX_PLAINTEXT_BYTES,
            )));
        }
        let p256dh = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(p256dh_b64url)
            .map_err(|e| WebPushError::Encryption(format!("p256dh base64: {e}")))?;
        if p256dh.len() != P256DH_KEY_LEN {
            return Err(WebPushError::Encryption(format!(
                "p256dh must decode to {P256DH_KEY_LEN} bytes (got {})",
                p256dh.len()
            )));
        }
        // RFC 8291 §3.1: an uncompressed SEC1 P-256 point starts with the
        // 0x04 tag. The browser `getKey('p256dh')` export is always
        // uncompressed; a compressed (0x02/0x03) or otherwise-tagged blob
        // means the stored subscription is corrupt or fabricated. ece
        // would surface this as a generic crypto error; surfacing it here
        // with the tag in hex makes the failure actionable.
        if p256dh[0] != 0x04 {
            return Err(WebPushError::Encryption(format!(
                "p256dh must be an uncompressed SEC1 point (tag 0x04, got 0x{:02x})",
                p256dh[0]
            )));
        }
        let auth = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(auth_b64url)
            .map_err(|e| WebPushError::Encryption(format!("auth base64: {e}")))?;
        if auth.len() != AUTH_SECRET_LEN {
            return Err(WebPushError::Encryption(format!(
                "auth secret must decode to {AUTH_SECRET_LEN} bytes (got {})",
                auth.len()
            )));
        }

        let body = match encoding {
            ContentEncoding::Aes128Gcm => ece::encrypt(&p256dh, &auth, plaintext)
                .map_err(|e| WebPushError::Encryption(format!("ece: {e}")))?,
        };
        Ok(Self {
            body,
            content_encoding: encoding,
        })
    }
}
