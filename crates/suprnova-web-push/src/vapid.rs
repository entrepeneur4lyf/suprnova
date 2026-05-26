//! VAPID — Voluntary Application Server Identification (RFC 8292).
//!
//! ES256 (P-256 ECDSA) signing per spec. Uses `jwt-simple` with the
//! `pure-rust` feature so we don't pull OpenSSL/BoringSSL.

use crate::error::WebPushError;
use base64::Engine;
use jwt_simple::prelude::*;
use serde::{Deserialize, Serialize};

/// A P-256 keypair for VAPID.
#[derive(Debug)]
pub struct VapidKey {
    inner: ES256KeyPair,
}

impl VapidKey {
    pub fn generate() -> Self {
        Self {
            inner: ES256KeyPair::generate(),
        }
    }

    pub fn from_pem(pem: &str) -> Result<Self, WebPushError> {
        let kp = ES256KeyPair::from_pem(pem)
            .map_err(|e| WebPushError::Vapid(format!("invalid PEM: {e}")))?;
        Ok(Self { inner: kp })
    }

    pub fn to_pem(&self) -> Result<String, WebPushError> {
        self.inner
            .to_pem()
            .map_err(|e| WebPushError::Vapid(format!("export PEM: {e}")))
    }

    /// Return the uncompressed public key (0x04 || X || Y), base64url-no-pad.
    /// The uncompressed encoding is 65 bytes → 87 base64url chars.
    pub fn public_key_uncompressed_b64url(&self) -> String {
        let pk = self.inner.public_key();
        // ECDSAP256PublicKeyLike::public_key() returns &P256PublicKey which has
        // to_bytes_uncompressed() — the ES256PublicKey wrapper only exposes
        // to_bytes() (compressed). We go through the trait to get the inner key.
        let raw = pk.public_key().to_bytes_uncompressed();
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
    }
}

/// Custom claims payload. Kept as a named type for callers that need to
/// construct or inspect VAPID claims directly; not used in `VapidSigner::sign`
/// to avoid duplicate standard-claim keys in the JWT payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VapidClaims {
    pub aud: String,
    pub exp: i64,
    pub sub: String,
}

#[derive(Debug)]
pub struct VapidSigner {
    key: VapidKey,
}

impl VapidSigner {
    pub fn new(key: VapidKey) -> Self {
        Self { key }
    }

    /// Sign a VAPID JWT.
    ///
    /// `audience` — push service origin, e.g. `"https://fcm.googleapis.com"`.
    /// `subject` — contact URI, e.g. `"mailto:admin@example.org"`.
    /// `ttl_secs` — token lifetime in seconds (max 24 h per RFC 8292).
    pub fn sign(
        &self,
        audience: &str,
        subject: &str,
        ttl_secs: i64,
    ) -> Result<String, WebPushError> {
        // Use standard claim helpers — avoids duplicate aud/sub/exp keys that
        // would occur if we embedded those fields in a custom claims struct
        // and also let jwt-simple set them via JWTClaims.
        //
        // Drop `nbf` (invalid_before): jwt-simple sets it to "now" by default.
        // Push services with even a few seconds of negative clock skew reject
        // the JWT before nbf passes. RFC 8292 requires only aud/sub/exp; iat
        // is optional but commonly included for replay-window tracking.
        // Final claim set: {iat, exp, sub, aud}.
        let mut claims = Claims::create(Duration::from_secs(ttl_secs as u64))
            .with_audience(audience)
            .with_subject(subject);
        claims.invalid_before = None;
        let jwt = self
            .key
            .inner
            .sign(claims)
            .map_err(|e| WebPushError::Vapid(format!("JWT sign: {e}")))?;
        Ok(jwt)
    }

    pub fn public_key_b64url(&self) -> String {
        self.key.public_key_uncompressed_b64url()
    }
}
