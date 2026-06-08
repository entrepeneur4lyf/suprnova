//! Session middleware validates the SHAPE of a decrypted session id
//! before letting it reach the session store.
//!
//! Even though the inbound cookie is AES-256-GCM authenticated, the
//! decrypted plaintext is still attacker-controlled in any scenario
//! where the key was rotated and the old ciphertext can still be
//! decrypted under a previous key, or when a key has been compromised.
//! The middleware must gate the id through
//! `super::store::is_valid_session_id` (40 lowercase-alphanumeric) and
//! mint a fresh id when the shape does not match — never let an
//! arbitrary string reach `SessionStore::read`.

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::Mutex;
use suprnova::session::{
    SessionConfig, SessionData, SessionMiddleware, SessionStore, is_valid_session_id,
};
use suprnova::{Crypt, EncryptionKey, FrameworkError};

fn ensure_crypt() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        Crypt::init(EncryptionKey::generate());
    });
}

/// Session store that records every id passed to `read()`. Always
/// returns `Ok(None)` so the middleware mints a new session.
struct RecordingStore {
    reads: Arc<Mutex<Vec<String>>>,
}

impl RecordingStore {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let reads = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                reads: reads.clone(),
            },
            reads,
        )
    }
}

#[async_trait]
impl SessionStore for RecordingStore {
    async fn read(&self, id: &str) -> Result<Option<SessionData>, FrameworkError> {
        self.reads.lock().unwrap().push(id.to_string());
        Ok(None)
    }
    async fn write(&self, _session: &SessionData) -> Result<(), FrameworkError> {
        Ok(())
    }
    async fn destroy(&self, _id: &str) -> Result<(), FrameworkError> {
        Ok(())
    }
    async fn destroy_for_user(&self, _user_id: &str) -> Result<u64, FrameworkError> {
        Ok(0)
    }
    async fn gc(&self) -> Result<u64, FrameworkError> {
        Ok(0)
    }
}

/// Minimal percent-encoder for cookie values used in test inputs.
///
/// `parse_cookies` percent-decodes the value, so a base64 wire that
/// contains `=` / `+` / `/` must be URL-encoded here to round-trip.
/// We only need to escape the small set of base64-extra characters
/// (`=`, `+`, `/`) plus the cookie-header separators (`;`, ` `).
fn percent_encode_cookie_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'=' => out.push_str("%3D"),
            b'+' => out.push_str("%2B"),
            b'/' => out.push_str("%2F"),
            b';' => out.push_str("%3B"),
            b' ' => out.push_str("%20"),
            b',' => out.push_str("%2C"),
            _ => out.push(b as char),
        }
    }
    out
}

/// Build a real `Request` (GET /) that carries a single inbound cookie
/// `cookie_value` under the configured session cookie name. Uses the
/// duplex-pipe + hyper harness pattern from
/// `session_persistence_fail_closed.rs`.
async fn get_request_with_session_cookie(
    cookie_name: &str,
    cookie_value: &str,
) -> suprnova::Request {
    let cookie_value = percent_encode_cookie_value(cookie_value);
    let cookie_value = &cookie_value;
    use bytes::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use suprnova::Request;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::oneshot;

    let cookie_header = format!("Cookie: {cookie_name}={cookie_value}\r\n");
    let http_bytes =
        format!("GET / HTTP/1.1\r\nHost: localhost\r\n{cookie_header}Content-Length: 0\r\n\r\n")
            .into_bytes();

    let (req_tx, req_rx) = oneshot::channel::<Request>();
    let req_tx = std::sync::Mutex::new(Some(req_tx));
    let (client_io, server_io) = tokio::io::duplex(http_bytes.len() + 64 * 1024);

    tokio::spawn(async move {
        let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
            let wrapped = Request::new(req);
            if let Ok(mut guard) = req_tx.lock()
                && let Some(tx) = guard.take()
            {
                let _ = tx.send(wrapped);
            }
            async {
                std::future::pending::<()>().await;
                Ok::<_, Infallible>(hyper::Response::new(http_body_util::Empty::<Bytes>::new()))
            }
        });
        let _ = http1::Builder::new()
            .serve_connection(TokioIo::new(server_io), svc)
            .await;
    });

    {
        let mut client = client_io;
        client.write_all(&http_bytes).await.unwrap();
    }
    req_rx.await.expect("server received request")
}

fn test_config() -> SessionConfig {
    SessionConfig {
        cookie_secure: false,
        ..SessionConfig::default()
    }
}

/// A cookie whose ciphertext decrypts to a string that does NOT match
/// `is_valid_session_id` must NOT reach `SessionStore::read`. The
/// middleware mints a fresh, shape-valid id instead.
///
/// This is the regression that closes the gap: prior to the fix,
/// `read()` saw the attacker-controlled string verbatim, opening a
/// path for arbitrary-id lookups (which can collide with internal
/// keys, exceed column widths, or otherwise drive surprising behaviour
/// in custom session stores).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_shape_id_is_replaced_with_fresh_id_before_reaching_store() {
    use suprnova::HttpResponse;
    use suprnova::http::cookie::Cookie;
    use suprnova::middleware::{Middleware, Next};

    ensure_crypt();

    // Construct a cookie carrying a plainly-invalid id (wrong length,
    // wrong charset). Cookie::encrypted produces a real GCM-authenticated
    // wire — the middleware will successfully DECRYPT it, but the
    // shape gate must then reject the plaintext and mint a fresh id.
    let bad_id = "FOO";
    let cookie = Cookie::encrypted("suprnova_session", bad_id).unwrap();
    let raw_value: String = cookie.value().to_string();

    let (store, reads) = RecordingStore::new();
    let middleware = SessionMiddleware::with_store(test_config(), Arc::new(store));

    let next: Next = Arc::new(move |_req| Box::pin(async move { Ok(HttpResponse::text("ok")) }));

    let req = get_request_with_session_cookie("suprnova_session", &raw_value).await;
    let _response = middleware.handle(req, next).await;

    let recorded = reads.lock().unwrap().clone();
    assert_eq!(
        recorded.len(),
        1,
        "middleware must consult the store exactly once"
    );
    let seen = &recorded[0];
    assert_ne!(
        seen, bad_id,
        "attacker-controlled cookie plaintext must never reach SessionStore::read"
    );
    assert!(
        is_valid_session_id(seen),
        "fallback id must match the canonical generate_session_id shape; got {seen:?}"
    );
}

/// A cookie carrying a SHAPE-VALID id must pass straight through to
/// the store — the validation gate is for malformed ids only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_shape_id_is_forwarded_to_store_untouched() {
    use suprnova::HttpResponse;
    use suprnova::http::cookie::Cookie;
    use suprnova::middleware::{Middleware, Next};

    ensure_crypt();

    let good_id = "a".repeat(40);
    assert!(is_valid_session_id(&good_id));
    let cookie = Cookie::encrypted("suprnova_session", &good_id).unwrap();
    let raw_value: String = cookie.value().to_string();

    let (store, reads) = RecordingStore::new();
    let middleware = SessionMiddleware::with_store(test_config(), Arc::new(store));

    let next: Next = Arc::new(move |_req| Box::pin(async move { Ok(HttpResponse::text("ok")) }));

    let req = get_request_with_session_cookie("suprnova_session", &raw_value).await;
    let _response = middleware.handle(req, next).await;

    let recorded = reads.lock().unwrap().clone();
    assert_eq!(recorded, vec![good_id.clone()]);
}
