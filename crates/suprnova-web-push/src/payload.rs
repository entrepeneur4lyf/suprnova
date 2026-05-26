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
        let auth = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(auth_b64url)
            .map_err(|e| WebPushError::Encryption(format!("auth base64: {e}")))?;

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
