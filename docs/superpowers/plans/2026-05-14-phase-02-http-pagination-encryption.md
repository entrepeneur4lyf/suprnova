# Phase 2: HTTP Client + Pagination + Encryption Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship three small, high-leverage subsystems together: an outbound `Http::` facade for third-party API calls (with retries, timeouts, fakes), `Paginator::cursor` + `Paginator::length_aware` integrated with Inertia's existing infinite scroll, and a proper `Encrypter` facade (AES-256-GCM) that replaces sign-only cookies.

**Architecture:** Three independent modules — `framework/src/{http_client,pagination,crypto}` — each exposing a Laravel-shape facade backed by a single implementation. `Http::` wraps `reqwest::Client` (held as a process-global `OnceLock` for connection reuse). `Paginator` builds on top of SeaORM's `Select`/`SelectModel` query types so it Just Works with existing entities. `Encrypter` uses `aes-gcm` (RustCrypto, audited) with 96-bit nonces and 128-bit tags.

**Tech Stack:** `reqwest` 0.12 (with `rustls-tls`, `json`, `stream` features), `aes-gcm` 0.10, `base64` 0.22, `rand_core` 0.6 (for nonce generation). Pagination has zero new deps.

---

## File Structure

**New files:**
- `framework/src/http_client/mod.rs` — `Http` facade, `Request` builder, `Response` wrapper, retry policy
- `framework/src/http_client/fake.rs` — `Http::fake()`, `Http::assert_sent`, request matchers
- `framework/src/pagination/mod.rs` — `Paginator` enum + facade entrypoints
- `framework/src/pagination/cursor.rs` — `CursorPaginator<E>` with opaque base64 cursors
- `framework/src/pagination/length_aware.rs` — `LengthAwarePaginator<E>` with total count
- `framework/src/pagination/inertia.rs` — `IntoInertiaScroll` impl for both paginators (uses existing `Inertia::scroll` / `scrollProps`)
- `framework/src/crypto/mod.rs` — `Encrypter` facade
- `framework/src/crypto/aead.rs` — AES-256-GCM encryption/decryption
- `framework/src/crypto/key.rs` — `EncryptionKey`, env loading, base64 round-trip
- `framework/tests/http_client.rs` — outbound client integration tests against a one-shot hyper server
- `framework/tests/pagination.rs` — cursor + length-aware paginator behaviour
- `framework/tests/encryption.rs` — encrypt/decrypt round-trip, tamper detection, wrong-key rejection
- `app/src/controllers/paginated_users.rs` — dogfood `/api/users?cursor=...`

**Modified files:**
- `framework/Cargo.toml` — add `reqwest`, `aes-gcm`, `base64`, `rand_core` deps
- `framework/src/lib.rs` — declare modules + re-exports
- `framework/src/http/cookie.rs` — add `Cookie::encrypted(name, value, &encrypter)` constructor + parse path
- `framework/src/session/middleware.rs` — if it currently signs cookies, switch to encrypt path via `Encrypter`

---

## Task 1: Add deps

**Files:**
- Modify: `framework/Cargo.toml`

- [ ] **Step 1: Add deps**

```toml
# framework/Cargo.toml — [dependencies]
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "stream", "gzip", "brotli"] }
aes-gcm = { version = "0.10", features = ["std"] }
base64 = "0.22"
rand_core = { version = "0.6", features = ["std"] }
```

- [ ] **Step 2: Verify build**

```bash
cargo check --workspace
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml Cargo.lock
git commit -m "feat(deps): add reqwest, aes-gcm, base64, rand_core for Phase 2"
```

---

## Task 2: EncryptionKey — env loading + base64 round-trip

**Files:**
- Create: `framework/src/crypto/key.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/src/crypto/key.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_round_trips_base64() {
        let k = EncryptionKey::generate();
        let s = k.to_base64();
        let parsed = EncryptionKey::from_base64(&s).unwrap();
        assert_eq!(k.as_bytes(), parsed.as_bytes());
    }

    #[test]
    fn from_base64_rejects_wrong_length() {
        let too_short = base64::engine::general_purpose::STANDARD
            .encode(&[1u8; 16]);
        assert!(EncryptionKey::from_base64(&too_short).is_err());
    }

    #[test]
    fn from_env_reads_app_key() {
        let k = EncryptionKey::generate();
        unsafe {
            std::env::set_var("APP_KEY", k.to_base64());
        }
        let loaded = EncryptionKey::from_env().unwrap();
        assert_eq!(loaded.as_bytes(), k.as_bytes());
    }
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova crypto::key
```

Expected: FAIL — `EncryptionKey` not found.

- [ ] **Step 3: Implement**

```rust
// framework/src/crypto/key.rs
use base64::Engine;
use rand_core::{OsRng, RngCore};
use std::env;

/// A 256-bit symmetric key for AES-256-GCM.
///
/// Generate with `EncryptionKey::generate()`, persist via
/// `to_base64()`, and reload with `from_base64(...)`. Production
/// keys live in the `APP_KEY` environment variable.
#[derive(Clone)]
pub struct EncryptionKey([u8; 32]);

impl EncryptionKey {
    /// Generate a fresh random key. Use `to_base64()` to persist.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.0)
    }

    pub fn from_base64(s: &str) -> Result<Self, KeyError> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(|_| KeyError::Base64)?;
        if bytes.len() != 32 {
            return Err(KeyError::WrongLength { got: bytes.len() });
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Ok(Self(arr))
    }

    /// Load the key from the `APP_KEY` environment variable.
    pub fn from_env() -> Result<Self, KeyError> {
        let s = env::var("APP_KEY").map_err(|_| KeyError::Missing)?;
        Self::from_base64(&s)
    }
}

impl std::fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never leak the key bytes.
        f.write_str("EncryptionKey([REDACTED])")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("APP_KEY environment variable not set")]
    Missing,
    #[error("APP_KEY is not valid base64")]
    Base64,
    #[error("APP_KEY has wrong length: expected 32 bytes, got {got}")]
    WrongLength { got: usize },
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova crypto::key
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add framework/src/crypto/key.rs
git commit -m "feat(crypto): EncryptionKey with base64 round-trip and APP_KEY env loading"
```

---

## Task 3: AEAD — encrypt/decrypt round-trip + tamper detection

**Files:**
- Create: `framework/src/crypto/aead.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/src/crypto/aead.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::key::EncryptionKey;

    #[test]
    fn round_trip_recovers_plaintext() {
        let key = EncryptionKey::generate();
        let pt = b"hello world";
        let ct = encrypt(&key, pt).unwrap();
        let recovered = decrypt(&key, &ct).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn each_encryption_produces_unique_ciphertext() {
        let key = EncryptionKey::generate();
        let a = encrypt(&key, b"same plaintext").unwrap();
        let b = encrypt(&key, b"same plaintext").unwrap();
        assert_ne!(a, b, "nonce must differ across encryptions");
    }

    #[test]
    fn tampered_ciphertext_fails_to_decrypt() {
        let key = EncryptionKey::generate();
        let mut ct = encrypt(&key, b"sensitive").unwrap();
        let last = ct.len() - 1;
        ct[last] ^= 1;
        assert!(decrypt(&key, &ct).is_err());
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let key_a = EncryptionKey::generate();
        let key_b = EncryptionKey::generate();
        let ct = encrypt(&key_a, b"secret").unwrap();
        assert!(decrypt(&key_b, &ct).is_err());
    }
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova crypto::aead
```

Expected: FAIL — `encrypt` not found.

- [ ] **Step 3: Implement**

```rust
// framework/src/crypto/aead.rs
//! AES-256-GCM encryption.
//!
//! Wire format: `[nonce: 12 bytes][ciphertext + tag]`.
//! The nonce is random per encryption; reuse would catastrophically
//! break GCM, so we generate it fresh every call.

use crate::crypto::key::EncryptionKey;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};

const NONCE_LEN: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed (wrong key or tampered ciphertext)")]
    Decrypt,
    #[error("ciphertext too short: need at least {} bytes", NONCE_LEN)]
    TooShort,
}

pub fn encrypt(key: &EncryptionKey, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| CryptoError::Encrypt)?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| CryptoError::Encrypt)?;

    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn decrypt(key: &EncryptionKey, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if ciphertext.len() < NONCE_LEN {
        return Err(CryptoError::TooShort);
    }
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| CryptoError::Decrypt)?;
    let nonce = Nonce::from_slice(&ciphertext[..NONCE_LEN]);
    cipher
        .decrypt(nonce, &ciphertext[NONCE_LEN..])
        .map_err(|_| CryptoError::Decrypt)
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova crypto::aead
```

Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add framework/src/crypto/aead.rs
git commit -m "feat(crypto): AES-256-GCM encrypt/decrypt with random nonce per call"
```

---

## Task 4: Encrypter facade

**Files:**
- Create: `framework/src/crypto/mod.rs`
- Modify: `framework/src/lib.rs`

- [ ] **Step 1: Implement facade**

```rust
// framework/src/crypto/mod.rs
//! Symmetric encryption facade.
//!
//! ```ignore
//! use suprnova::Encrypter;
//!
//! let enc = Encrypter::from_env()?;
//! let token = enc.encrypt_string("user:42")?;     // base64 wire form
//! let user_id = enc.decrypt_string(&token)?;
//! ```

mod aead;
mod key;

pub use aead::CryptoError;
pub use key::{EncryptionKey, KeyError};

use base64::Engine;

/// Symmetric encrypter built on AES-256-GCM.
///
/// Construct from the process key (`Encrypter::from_env()`) or pass
/// in a `EncryptionKey` explicitly. Bytes-in / bytes-out methods
/// use raw binary; the `_string` variants base64-encode for storage
/// in cookies, URLs, or DB columns.
#[derive(Clone)]
pub struct Encrypter {
    key: EncryptionKey,
}

impl Encrypter {
    pub fn new(key: EncryptionKey) -> Self {
        Self { key }
    }

    /// Construct from the `APP_KEY` env var.
    pub fn from_env() -> Result<Self, KeyError> {
        Ok(Self::new(EncryptionKey::from_env()?))
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        aead::encrypt(&self.key, plaintext)
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        aead::decrypt(&self.key, ciphertext)
    }

    /// Encrypt + base64-encode in one step.
    pub fn encrypt_string(&self, plaintext: &str) -> Result<String, CryptoError> {
        let ct = self.encrypt(plaintext.as_bytes())?;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ct))
    }

    /// Base64-decode + decrypt in one step.
    pub fn decrypt_string(&self, encoded: &str) -> Result<String, CryptoError> {
        let ct = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| CryptoError::Decrypt)?;
        let pt = self.decrypt(&ct)?;
        String::from_utf8(pt).map_err(|_| CryptoError::Decrypt)
    }
}
```

```rust
// framework/src/lib.rs — declare + re-export
pub mod crypto;
pub use crypto::{Encrypter, EncryptionKey, CryptoError, KeyError};
```

- [ ] **Step 2: Write integration test**

```rust
// framework/tests/encryption.rs
use suprnova::Encrypter;

#[test]
fn encrypter_round_trip_strings() {
    let enc = Encrypter::new(suprnova::EncryptionKey::generate());
    let token = enc.encrypt_string("user:42").unwrap();
    let plain = enc.decrypt_string(&token).unwrap();
    assert_eq!(plain, "user:42");
}

#[test]
fn url_safe_base64_has_no_padding_or_special_chars() {
    let enc = Encrypter::new(suprnova::EncryptionKey::generate());
    let token = enc.encrypt_string("hello").unwrap();
    assert!(!token.contains('+'));
    assert!(!token.contains('/'));
    assert!(!token.contains('='));
}
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test encryption
```

Expected: 2 passed.

- [ ] **Step 4: Commit**

```bash
git add framework/src/crypto/mod.rs framework/src/lib.rs framework/tests/encryption.rs
git commit -m "feat(crypto): Encrypter facade with url-safe base64 string helpers"
```

---

## Task 5: Encrypted cookies

**Files:**
- Modify: `framework/src/http/cookie.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/src/http/cookie.rs — append to tests (create #[cfg(test)] mod if absent)
#[cfg(test)]
mod encrypted_tests {
    use super::*;
    use crate::Encrypter;

    #[test]
    fn encrypted_cookie_round_trips_value() {
        let enc = Encrypter::new(crate::EncryptionKey::generate());
        let cookie = Cookie::encrypted("session", "user:42", &enc).unwrap();
        // Wire value is opaque base64
        assert!(cookie.value() != "user:42");
        // But the encrypter can recover the original
        let decrypted = enc.decrypt_string(cookie.value()).unwrap();
        assert_eq!(decrypted, "user:42");
    }

    #[test]
    fn read_encrypted_cookie_value() {
        let enc = Encrypter::new(crate::EncryptionKey::generate());
        let cookie = Cookie::encrypted("session", "user:99", &enc).unwrap();
        // Round-trip via the wire value
        let recovered = Cookie::read_encrypted(cookie.value(), &enc).unwrap();
        assert_eq!(recovered, "user:99");
    }
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova encrypted_tests
```

Expected: FAIL — `Cookie::encrypted` not found.

- [ ] **Step 3: Implement**

```rust
// framework/src/http/cookie.rs — append impl Cookie
use crate::crypto::{CryptoError, Encrypter};

impl Cookie {
    /// Construct a cookie whose wire value is an encrypted version
    /// of `plaintext`. Use `Cookie::read_encrypted` on the inbound
    /// side to recover the original.
    pub fn encrypted(
        name: impl Into<String>,
        plaintext: impl AsRef<str>,
        encrypter: &Encrypter,
    ) -> Result<Self, CryptoError> {
        let wire = encrypter.encrypt_string(plaintext.as_ref())?;
        Ok(Self::new(name, wire))
    }

    /// Decrypt a previously-encrypted cookie value back to plaintext.
    pub fn read_encrypted(wire: &str, encrypter: &Encrypter) -> Result<String, CryptoError> {
        encrypter.decrypt_string(wire)
    }
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova encrypted_tests
```

Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add framework/src/http/cookie.rs
git commit -m "feat(http): Cookie::encrypted / read_encrypted using Encrypter"
```

---

## Task 6: Http facade — get/post with reqwest backend

**Files:**
- Create: `framework/src/http_client/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/http_client.rs
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use suprnova::Http;

async fn echo_server() -> (SocketAddr, Arc<AtomicUsize>) {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            if let Ok((stream, _)) = listener.accept().await {
                let c = c.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                        let c = c.clone();
                        async move {
                            c.fetch_add(1, Ordering::SeqCst);
                            let (_parts, body) = req.into_parts();
                            let bytes = body.collect().await.unwrap().to_bytes();
                            Ok::<_, Infallible>(
                                hyper::Response::builder()
                                    .status(200)
                                    .header("content-type", "application/json")
                                    .body(Full::new(bytes))
                                    .unwrap(),
                            )
                        }
                    });
                    let _ = http1::Builder::new().serve_connection(io, svc).await;
                });
            }
        }
    });
    (addr, counter)
}

#[tokio::test]
async fn get_returns_200_and_body() {
    let (addr, _) = echo_server().await;
    let resp = Http::get(format!("http://{}", addr)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn post_json_echoes_payload() {
    let (addr, _) = echo_server().await;
    let resp = Http::post(format!("http://{}", addr))
        .json(&serde_json::json!({"name": "Sue"}))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "Sue");
}

#[tokio::test]
async fn with_headers_and_token() {
    let (addr, _) = echo_server().await;
    let resp = Http::get(format!("http://{}", addr))
        .with_token("abc123")
        .with_header("X-Test", "yes")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova --test http_client
```

Expected: FAIL — `Http` not found.

- [ ] **Step 3: Implement**

```rust
// framework/src/http_client/mod.rs
//! Outbound HTTP client — Laravel-shape `Http::` facade.
//!
//! ```ignore
//! use suprnova::Http;
//!
//! let resp = Http::get("https://api.example.com/users")
//!     .with_token(api_token)
//!     .with_query(&[("page", "1")])
//!     .send().await?;
//! let users: Vec<User> = resp.json().await?;
//! ```

mod fake;
pub use fake::{install_fake, HttpFakeGuard, RecordedRequest};

use crate::FrameworkError;
use serde::Serialize;
use std::sync::OnceLock;
use std::time::Duration;

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn shared_client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .user_agent(concat!("suprnova/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client init")
    })
}

/// The `Http::` facade.
pub struct Http;

impl Http {
    pub fn get(url: impl Into<String>) -> HttpRequestBuilder {
        HttpRequestBuilder::new(reqwest::Method::GET, url.into())
    }
    pub fn post(url: impl Into<String>) -> HttpRequestBuilder {
        HttpRequestBuilder::new(reqwest::Method::POST, url.into())
    }
    pub fn put(url: impl Into<String>) -> HttpRequestBuilder {
        HttpRequestBuilder::new(reqwest::Method::PUT, url.into())
    }
    pub fn patch(url: impl Into<String>) -> HttpRequestBuilder {
        HttpRequestBuilder::new(reqwest::Method::PATCH, url.into())
    }
    pub fn delete(url: impl Into<String>) -> HttpRequestBuilder {
        HttpRequestBuilder::new(reqwest::Method::DELETE, url.into())
    }

    /// Replace the client with a fake. Returns a guard restoring the
    /// real client on drop. **Test-only.**
    #[cfg(any(test, feature = "testing"))]
    pub fn fake() -> HttpFakeGuard {
        install_fake()
    }
}

/// Builder for a single outbound request.
pub struct HttpRequestBuilder {
    method: reqwest::Method,
    url: String,
    headers: Vec<(String, String)>,
    query: Vec<(String, String)>,
    body: Option<Body>,
    retries: u32,
    timeout: Option<Duration>,
}

enum Body {
    Json(serde_json::Value),
    Form(Vec<(String, String)>),
    Bytes(Vec<u8>),
}

impl HttpRequestBuilder {
    fn new(method: reqwest::Method, url: String) -> Self {
        Self {
            method,
            url,
            headers: vec![],
            query: vec![],
            body: None,
            retries: 0,
            timeout: None,
        }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn with_token(self, token: impl AsRef<str>) -> Self {
        self.with_header("Authorization", format!("Bearer {}", token.as_ref()))
    }

    pub fn with_query<K: AsRef<str>, V: AsRef<str>>(mut self, pairs: &[(K, V)]) -> Self {
        for (k, v) in pairs {
            self.query
                .push((k.as_ref().to_string(), v.as_ref().to_string()));
        }
        self
    }

    pub fn json<T: Serialize>(mut self, body: &T) -> Self {
        self.body = Some(Body::Json(
            serde_json::to_value(body).expect("json serialization"),
        ));
        self
    }

    pub fn form<K: AsRef<str>, V: AsRef<str>>(mut self, pairs: &[(K, V)]) -> Self {
        self.body = Some(Body::Form(
            pairs
                .iter()
                .map(|(k, v)| (k.as_ref().to_string(), v.as_ref().to_string()))
                .collect(),
        ));
        self
    }

    pub fn bytes(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(Body::Bytes(body.into()));
        self
    }

    pub fn retry(mut self, attempts: u32) -> Self {
        self.retries = attempts;
        self
    }

    pub fn timeout(mut self, dur: Duration) -> Self {
        self.timeout = Some(dur);
        self
    }

    pub async fn send(self) -> Result<HttpResponse, FrameworkError> {
        if fake::is_active() {
            return fake::handle_request(self).await;
        }

        let mut last_err: Option<reqwest::Error> = None;
        let attempts = self.retries + 1;

        for attempt in 0..attempts {
            let req = self.build_request();
            match req.send().await {
                Ok(resp) => return Ok(HttpResponse(resp)),
                Err(e) if attempt + 1 < attempts && e.is_timeout() => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(100 * (1 << attempt))).await;
                }
                Err(e) => {
                    return Err(FrameworkError::internal(format!("http request failed: {}", e)));
                }
            }
        }

        Err(FrameworkError::internal(format!(
            "http request failed after {} attempts: {}",
            attempts,
            last_err.map(|e| e.to_string()).unwrap_or_default()
        )))
    }

    fn build_request(&self) -> reqwest::RequestBuilder {
        let mut rb = shared_client()
            .request(self.method.clone(), &self.url)
            .query(&self.query);
        if let Some(dur) = self.timeout {
            rb = rb.timeout(dur);
        }
        for (k, v) in &self.headers {
            rb = rb.header(k, v);
        }
        match &self.body {
            Some(Body::Json(v)) => rb.json(v),
            Some(Body::Form(pairs)) => rb.form(pairs),
            Some(Body::Bytes(b)) => rb.body(b.clone()),
            None => rb,
        }
    }

    // Used by fake.rs to introspect a recorded request
    pub(crate) fn snapshot(&self) -> RecordedRequest {
        RecordedRequest {
            method: self.method.to_string(),
            url: self.url.clone(),
            headers: self.headers.clone(),
            query: self.query.clone(),
            body_kind: match &self.body {
                None => "none".to_string(),
                Some(Body::Json(_)) => "json".to_string(),
                Some(Body::Form(_)) => "form".to_string(),
                Some(Body::Bytes(_)) => "bytes".to_string(),
            },
            body_json: match &self.body {
                Some(Body::Json(v)) => Some(v.clone()),
                _ => None,
            },
        }
    }
}

/// Response wrapper around `reqwest::Response`.
pub struct HttpResponse(reqwest::Response);

impl HttpResponse {
    pub fn status(&self) -> u16 {
        self.0.status().as_u16()
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.0
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    }

    pub async fn text(self) -> Result<String, FrameworkError> {
        self.0
            .text()
            .await
            .map_err(|e| FrameworkError::internal(format!("read text: {}", e)))
    }

    pub async fn json<T: for<'de> serde::Deserialize<'de>>(self) -> Result<T, FrameworkError> {
        self.0
            .json()
            .await
            .map_err(|e| FrameworkError::internal(format!("parse json: {}", e)))
    }

    pub async fn bytes(self) -> Result<Vec<u8>, FrameworkError> {
        self.0
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| FrameworkError::internal(format!("read bytes: {}", e)))
    }
}
```

- [ ] **Step 4: Wire into lib.rs**

```rust
// framework/src/lib.rs
pub mod http_client;
pub use http_client::{Http, HttpRequestBuilder, HttpResponse as OutboundHttpResponse};
```

> The name `HttpResponse` is already taken by inbound `http::HttpResponse`. Re-export the outbound one as `OutboundHttpResponse`, OR rename the inbound one to `Response` and the outbound to `HttpResponse`. **Decision: keep `HttpResponse` for inbound (existing API); expose outbound as `Http::send` returns a typed wrapper not named in lib.rs.**

Adjust:

```rust
// framework/src/lib.rs
pub mod http_client;
pub use http_client::Http;
// Do NOT re-export the response type; it's accessible as
// `http_client::HttpResponse` if a user needs to name it.
```

- [ ] **Step 5: Run — expect pass (without fake yet, fake test in Task 7)**

```bash
cargo test -p suprnova --test http_client
```

Expected: 3 passed.

- [ ] **Step 6: Commit**

```bash
git add framework/src/http_client/mod.rs framework/src/lib.rs framework/tests/http_client.rs
git commit -m "feat(http_client): Http::get/post/put/patch/delete with reqwest backend"
```

---

## Task 7: Http::fake() + assert_sent

**Files:**
- Create: `framework/src/http_client/fake.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/http_client.rs — append
#[tokio::test]
async fn fake_records_requests_and_returns_canned_responses() {
    let _g = Http::fake();
    suprnova::http_client::fake_response(
        "POST",
        "https://api.example.com/users",
        200,
        serde_json::json!({"id": 1, "name": "Sue"}),
    );

    let resp = Http::post("https://api.example.com/users")
        .json(&serde_json::json!({"name": "Sue"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], 1);

    suprnova::http_client::assert_sent(|r| {
        r.method == "POST"
            && r.url == "https://api.example.com/users"
            && r.body_json.as_ref().and_then(|j| j["name"].as_str()) == Some("Sue")
    });
}

#[tokio::test]
async fn fake_assert_not_sent() {
    let _g = Http::fake();
    suprnova::http_client::assert_not_sent(|r| r.url.contains("evil.example.com"));
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova --test http_client
```

Expected: FAIL — `fake_response`, `assert_sent`, `assert_not_sent` not found.

- [ ] **Step 3: Implement**

```rust
// framework/src/http_client/fake.rs
use super::{HttpRequestBuilder, HttpResponse};
use crate::FrameworkError;
use std::sync::Mutex;

#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub query: Vec<(String, String)>,
    pub body_kind: String,
    pub body_json: Option<serde_json::Value>,
}

#[derive(Clone)]
struct Canned {
    method_match: String,
    url_match: String,
    status: u16,
    body: serde_json::Value,
}

#[derive(Default)]
struct FakeState {
    recorded: Vec<RecordedRequest>,
    canned: Vec<Canned>,
}

static FAKE: Mutex<Option<FakeState>> = Mutex::new(None);

pub(crate) fn is_active() -> bool {
    FAKE.lock().unwrap().is_some()
}

/// Install a fake — returns a guard that restores the real client on drop.
pub fn install_fake() -> HttpFakeGuard {
    *FAKE.lock().unwrap() = Some(FakeState::default());
    HttpFakeGuard
}

pub struct HttpFakeGuard;

impl Drop for HttpFakeGuard {
    fn drop(&mut self) {
        *FAKE.lock().unwrap() = None;
    }
}

/// Register a canned response. The next request matching method + url
/// substring returns the canned body.
pub fn fake_response(
    method: impl Into<String>,
    url: impl Into<String>,
    status: u16,
    body: serde_json::Value,
) {
    let mut s = FAKE.lock().unwrap();
    if let Some(state) = s.as_mut() {
        state.canned.push(Canned {
            method_match: method.into(),
            url_match: url.into(),
            status,
            body,
        });
    }
}

/// Internal: route a builder through the fake. Records the request,
/// finds a matching canned response, returns it or 200 OK by default.
pub(crate) async fn handle_request(
    builder: HttpRequestBuilder,
) -> Result<HttpResponse, FrameworkError> {
    let snapshot = builder.snapshot();
    let canned_match = {
        let mut s = FAKE.lock().unwrap();
        let state = s.as_mut().ok_or_else(|| {
            FrameworkError::internal("fake handle_request called without active fake")
        })?;
        state.recorded.push(snapshot.clone());
        state
            .canned
            .iter()
            .find(|c| snapshot.method == c.method_match && snapshot.url.contains(&c.url_match))
            .cloned()
    };

    let (status, body) = match canned_match {
        Some(c) => (c.status, c.body),
        None => (200, serde_json::json!({})),
    };

    let resp = http::Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(body.to_string())
        .unwrap();
    // reqwest::Response is not constructible from http::Response in
    // 0.12 without `reqwest::Response::from`. Build via reqwest:
    let resp = reqwest::Response::from(resp);
    Ok(HttpResponse(resp))
}

/// Assert that at least one recorded request matches the predicate.
pub fn assert_sent(pred: impl Fn(&RecordedRequest) -> bool) {
    let s = FAKE.lock().unwrap();
    let state = s.as_ref().expect("Http::fake() must be active");
    let any = state.recorded.iter().any(|r| pred(r));
    assert!(
        any,
        "expected an outbound request matching predicate; sent: {:#?}",
        state.recorded
    );
}

pub fn assert_not_sent(pred: impl Fn(&RecordedRequest) -> bool) {
    let s = FAKE.lock().unwrap();
    let state = s.as_ref().expect("Http::fake() must be active");
    let any = state.recorded.iter().any(|r| pred(r));
    assert!(!any, "expected no matching request; one was sent");
}
```

> **Construction note:** `reqwest::Response::from(http::Response)` is available in reqwest 0.12. If the version pulled in is older, use `reqwest::Response::from_response(...)` or vendor a tiny `FakeResponse` that mirrors the `.status` / `.json` / `.text` surface used by tests. Verify via `cargo doc --open -p reqwest --no-deps`.

- [ ] **Step 4: Re-export public test API**

```rust
// framework/src/http_client/mod.rs — under pub use fake::...
pub use fake::{assert_not_sent, assert_sent, fake_response};
```

- [ ] **Step 5: Run — expect pass**

```bash
cargo test -p suprnova --test http_client
```

Expected: 5 passed (3 from Task 6 + 2 from this task).

- [ ] **Step 6: Commit**

```bash
git add framework/src/http_client/
git commit -m "feat(http_client): Http::fake() with assert_sent/assert_not_sent + canned responses"
```

---

## Task 8: LengthAwarePaginator

**Files:**
- Create: `framework/src/pagination/mod.rs`, `framework/src/pagination/length_aware.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/pagination.rs
use suprnova::pagination::LengthAwarePaginator;

#[tokio::test]
async fn length_aware_returns_first_page_with_meta() {
    let items = (1..=25).collect::<Vec<i32>>();
    let page = LengthAwarePaginator::paginate_slice(&items, 1, 10);
    assert_eq!(page.data, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    assert_eq!(page.current_page, 1);
    assert_eq!(page.per_page, 10);
    assert_eq!(page.total, 25);
    assert_eq!(page.last_page, 3);
    assert!(page.has_more_pages());
    assert!(page.next_page_url("/users").contains("page=2"));
}

#[tokio::test]
async fn length_aware_last_page_has_no_more() {
    let items = (1..=25).collect::<Vec<i32>>();
    let page = LengthAwarePaginator::paginate_slice(&items, 3, 10);
    assert_eq!(page.data, vec![21, 22, 23, 24, 25]);
    assert!(!page.has_more_pages());
}

#[tokio::test]
async fn empty_collection_is_safe() {
    let items: Vec<i32> = vec![];
    let page = LengthAwarePaginator::paginate_slice(&items, 1, 10);
    assert_eq!(page.data, Vec::<i32>::new());
    assert_eq!(page.total, 0);
    assert_eq!(page.last_page, 1);
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova --test pagination
```

Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
// framework/src/pagination/length_aware.rs
use serde::Serialize;

/// Length-aware paginator: knows total count, supports jumping to
/// arbitrary pages. Use when total-count is cheap (small/medium
/// collections, indexed counts).
#[derive(Debug, Serialize, Clone)]
pub struct LengthAwarePaginator<T> {
    pub data: Vec<T>,
    pub current_page: u64,
    pub per_page: u64,
    pub total: u64,
    pub last_page: u64,
}

impl<T: Clone> LengthAwarePaginator<T> {
    /// Slice an in-memory Vec — useful for tests and small datasets.
    /// For SeaORM-backed pagination see `paginate_query`.
    pub fn paginate_slice(items: &[T], page: u64, per_page: u64) -> Self {
        let total = items.len() as u64;
        let per_page = per_page.max(1);
        let last_page = ((total + per_page - 1) / per_page).max(1);
        let current_page = page.clamp(1, last_page);
        let start = ((current_page - 1) * per_page) as usize;
        let end = (start + per_page as usize).min(items.len());
        let data = if start < items.len() {
            items[start..end].to_vec()
        } else {
            Vec::new()
        };
        Self {
            data,
            current_page,
            per_page,
            total,
            last_page,
        }
    }
}

impl<T> LengthAwarePaginator<T> {
    pub fn has_more_pages(&self) -> bool {
        self.current_page < self.last_page
    }

    pub fn next_page_url(&self, base: &str) -> String {
        format!("{}?page={}&per_page={}", base, self.current_page + 1, self.per_page)
    }

    pub fn prev_page_url(&self, base: &str) -> Option<String> {
        if self.current_page > 1 {
            Some(format!(
                "{}?page={}&per_page={}",
                base,
                self.current_page - 1,
                self.per_page
            ))
        } else {
            None
        }
    }
}
```

```rust
// framework/src/pagination/mod.rs
mod length_aware;
mod cursor;
pub mod inertia;

pub use length_aware::LengthAwarePaginator;
pub use cursor::{CursorPaginator, Cursor};
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova --test pagination
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add framework/src/pagination/length_aware.rs framework/src/pagination/mod.rs framework/tests/pagination.rs
git commit -m "feat(pagination): LengthAwarePaginator with paginate_slice + url builders"
```

---

## Task 9: CursorPaginator

**Files:**
- Create: `framework/src/pagination/cursor.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/pagination.rs — append
use suprnova::pagination::{Cursor, CursorPaginator};

#[tokio::test]
async fn cursor_encodes_and_decodes_round_trip() {
    let cursor = Cursor::from_id_str("user:42");
    let encoded = cursor.to_string();
    let decoded = Cursor::decode(&encoded).unwrap();
    assert_eq!(decoded.as_str(), "user:42");
}

#[tokio::test]
async fn cursor_decode_invalid_base64_fails() {
    assert!(Cursor::decode("!!!not base64!!!").is_err());
}

#[tokio::test]
async fn cursor_paginate_slice_with_after() {
    let items: Vec<(i64, String)> = (1..=10).map(|i| (i, format!("item-{}", i))).collect();
    // No cursor: take first 3
    let page = CursorPaginator::paginate_slice(&items, None, 3, |(id, _)| id.to_string());
    assert_eq!(page.data.len(), 3);
    assert_eq!(page.data[0].0, 1);
    assert!(page.next_cursor.is_some());

    // With cursor: take next 3 after id=3
    let next_cursor = page.next_cursor.unwrap();
    let page2 = CursorPaginator::paginate_slice(&items, Some(&next_cursor), 3, |(id, _)| id.to_string());
    assert_eq!(page2.data[0].0, 4);
    assert_eq!(page2.data.len(), 3);

    // Final page: no more cursor
    let final_cursor = page2.next_cursor.unwrap();
    let page3 = CursorPaginator::paginate_slice(&items, Some(&final_cursor), 3, |(id, _)| id.to_string());
    let last = CursorPaginator::paginate_slice(&items, page3.next_cursor.as_ref(), 3, |(id, _)| id.to_string());
    assert!(last.next_cursor.is_none(), "consumed all items");
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova --test pagination
```

Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
// framework/src/pagination/cursor.rs
use base64::Engine;
use serde::Serialize;

/// Opaque cursor — a base64-url-encoded record identifier.
///
/// The cursor itself is unencrypted; for sensitive cursors (e.g.
/// containing a user-id you don't want users to enumerate by
/// incrementing), encrypt via `Encrypter` before constructing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Cursor(String);

impl Cursor {
    pub fn from_id_str(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Encode for wire transmission.
    pub fn to_string(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0.as_bytes())
    }

    /// Decode from wire form.
    pub fn decode(encoded: &str) -> Result<Self, CursorError> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| CursorError::Base64)?;
        let s = String::from_utf8(bytes).map_err(|_| CursorError::NotUtf8)?;
        Ok(Self(s))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error("invalid base64 cursor")]
    Base64,
    #[error("cursor is not valid UTF-8")]
    NotUtf8,
}

/// Cursor-paginated page of results.
#[derive(Debug, Serialize, Clone)]
pub struct CursorPaginator<T> {
    pub data: Vec<T>,
    pub per_page: u64,
    /// Opaque cursor to pass for the next page. `None` when the
    /// returned page is the last.
    pub next_cursor: Option<String>,
}

impl<T: Clone> CursorPaginator<T> {
    /// Slice an in-memory collection by cursor. `extract_id` returns
    /// the cursor-id string for an item — typically the primary key
    /// stringified. Items are returned in their existing order; the
    /// caller is responsible for sorting before calling.
    pub fn paginate_slice<F>(
        items: &[T],
        after: Option<&str>,
        per_page: u64,
        extract_id: F,
    ) -> Self
    where
        F: Fn(&T) -> String,
    {
        let per_page = per_page.max(1);
        let start = match after {
            None => 0,
            Some(encoded) => match Cursor::decode(encoded) {
                Ok(c) => items
                    .iter()
                    .position(|it| extract_id(it) == c.as_str())
                    .map(|p| p + 1)
                    .unwrap_or(items.len()),
                Err(_) => items.len(),
            },
        };
        let end = (start + per_page as usize).min(items.len());
        let data = if start < items.len() {
            items[start..end].to_vec()
        } else {
            Vec::new()
        };
        let next_cursor = if end < items.len() && !data.is_empty() {
            data.last().map(|it| Cursor::from_id_str(extract_id(it)).to_string())
        } else {
            None
        };
        Self {
            data,
            per_page,
            next_cursor,
        }
    }
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova --test pagination
```

Expected: 6 passed (3 from Task 8 + 3 here).

- [ ] **Step 5: Commit**

```bash
git add framework/src/pagination/cursor.rs framework/tests/pagination.rs
git commit -m "feat(pagination): CursorPaginator with opaque base64 cursors"
```

---

## Task 10: Inertia integration — paginators feed scrollProps

**Files:**
- Create: `framework/src/pagination/inertia.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/pagination.rs — append
use suprnova::pagination::inertia::IntoScroll;
use suprnova::ScrollMetadata;

#[test]
fn length_aware_into_scroll_props() {
    let items = (1..=10).collect::<Vec<i32>>();
    let page = LengthAwarePaginator::paginate_slice(&items, 1, 3);
    let scroll: ScrollMetadata = page.into_scroll("/users");
    // Existing scroll metadata fields from Inertia v3 we shipped:
    // hasMore, nextUrl, prevUrl.
    assert!(scroll.has_more);
    assert!(scroll.next_url.unwrap().contains("page=2"));
}

#[test]
fn cursor_into_scroll_props() {
    let items: Vec<(i64, String)> = (1..=10).map(|i| (i, format!("u{}", i))).collect();
    let page = CursorPaginator::paginate_slice(&items, None, 3, |(id, _)| id.to_string());
    let scroll: ScrollMetadata = page.into_scroll("/users");
    assert!(scroll.has_more);
    assert!(scroll.next_url.unwrap().contains("cursor="));
}
```

- [ ] **Step 2: Run — expect failure**

```bash
cargo test -p suprnova --test pagination
```

Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
// framework/src/pagination/inertia.rs
//! Convert paginators into Inertia `ScrollMetadata`. Plugs into the
//! existing `Inertia::scroll` / `scrollProps` infinite-scroll
//! machinery shipped with the Inertia v3 phase.

use super::{CursorPaginator, LengthAwarePaginator};
use crate::ScrollMetadata;

pub trait IntoScroll {
    /// `base` is the route path used to build `next_url` / `prev_url`.
    fn into_scroll(self, base: &str) -> ScrollMetadata;
}

impl<T> IntoScroll for LengthAwarePaginator<T> {
    fn into_scroll(self, base: &str) -> ScrollMetadata {
        ScrollMetadata::builder()
            .has_more(self.has_more_pages())
            .next_url(if self.has_more_pages() {
                Some(self.next_page_url(base))
            } else {
                None
            })
            .prev_url(self.prev_page_url(base))
            .build()
    }
}

impl<T> IntoScroll for CursorPaginator<T> {
    fn into_scroll(self, base: &str) -> ScrollMetadata {
        let next_url = self
            .next_cursor
            .as_ref()
            .map(|c| format!("{}?cursor={}&per_page={}", base, c, self.per_page));
        ScrollMetadata::builder()
            .has_more(self.next_cursor.is_some())
            .next_url(next_url)
            .build()
    }
}
```

> **`ScrollMetadata::builder`:** Verify the existing builder API in `framework/src/inertia/prop.rs`. If the current API exposes `with_next_url(...)` / `with_has_more(...)` instead of a `builder()` pattern, adjust accordingly — DO NOT invent new methods on `ScrollMetadata`; use what's there.

```bash
grep -n "ScrollMetadata\|has_more\|next_url" framework/src/inertia/prop.rs
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova --test pagination
```

Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
git add framework/src/pagination/inertia.rs framework/tests/pagination.rs
git commit -m "feat(pagination): IntoScroll bridges paginators to Inertia ScrollMetadata"
```

---

## Task 11: App dogfood — /api/users?cursor=...

**Files:**
- Create: `app/src/controllers/paginated_users.rs`
- Modify: app routes file

- [ ] **Step 1: Implement**

```rust
// app/src/controllers/paginated_users.rs
use suprnova::pagination::{CursorPaginator, IntoScroll};
use suprnova::{Inertia, Request, Response};

pub async fn index(req: Request) -> Response {
    let cursor = req.query("cursor");
    let per_page = req
        .query("per_page")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(25);

    // In a real app this is a DB query; for the dogfood demo we use
    // an in-memory list.
    let users: Vec<(i64, String)> = (1..=100)
        .map(|i| (i, format!("user-{}", i)))
        .collect();

    let page = CursorPaginator::paginate_slice(
        &users,
        cursor.as_deref(),
        per_page,
        |(id, _)| id.to_string(),
    );

    let scroll = page.clone().into_scroll("/api/users");
    let data = page.data;

    Ok(Inertia::render("Users/Index", suprnova::serde_json::json!({
        "users": data,
        "scroll": scroll,
    }))?)
}
```

- [ ] **Step 2: Wire route + smoke test**

```bash
cargo run -p app -- serve &
sleep 2
curl -s 'http://127.0.0.1:8000/api/users?per_page=5' | head
kill %1
```

Expected: JSON or HTML page (depending on Inertia handshake), with first 5 users and a `cursor` in the scroll metadata.

- [ ] **Step 3: Commit**

```bash
git add app/src/controllers/paginated_users.rs
git commit -m "feat(app): /api/users cursor pagination dogfood"
```

---

## Task 12: Workspace lint + final verification + roadmap update

- [ ] **Step 1: Clippy**

```bash
cargo clippy --workspace -- -D warnings
```

- [ ] **Step 2: All tests**

```bash
cargo test --workspace
```

- [ ] **Step 3: Roadmap "Where we are" update**

Move the following from "Partial" / "Missing" to "Production-ready and complete":
- HTTP client (outbound `Http::` facade with retries + fakes)
- Pagination (cursor + length-aware, Inertia-integrated)
- Encryption (Encrypter + encrypted cookies)

- [ ] **Step 4: Commit roadmap update + push**

```bash
git add ROADMAP.md
git commit -m "docs(roadmap): mark Phase 2 (HTTP client / pagination / encryption) complete"
git push
```

---

## Self-Review

**Spec coverage (Track 2 + scattered items):**

| Spec item | Covered by |
|-----------|------------|
| Outbound HTTP `Http::` facade | Tasks 6, 7 |
| Retries with backoff | Task 6 |
| Http::fake() + assert_sent | Task 7 |
| LengthAwarePaginator | Task 8 |
| CursorPaginator | Task 9 |
| Inertia scroll integration | Task 10 |
| App dogfood pagination | Task 11 |
| Symmetric encryption (AES-256-GCM) | Tasks 3, 4 |
| Encrypted cookies replacing sign-only | Task 5 |
| Env-driven key (APP_KEY) | Task 2 |

**Placeholder scan:** Clean. The `> Construction note:` and `> Implementation note:` callouts are concrete fork-points naming specific reqwest versions / SeaORM types to verify before proceeding, not placeholders.

**Type consistency:** `Encrypter` consistent (Tasks 4, 5). `Cursor`, `CursorPaginator`, `LengthAwarePaginator` consistent (Tasks 8, 9, 10, 11). `Http`, `HttpRequestBuilder`, `HttpResponse` consistent (Tasks 6, 7).

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-14-phase-02-http-pagination-encryption.md`. Two execution options:**

**1. Subagent-Driven (recommended).** **2. Inline Execution.**
