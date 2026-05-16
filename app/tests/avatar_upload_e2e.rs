//! End-to-end tests for the avatar upload endpoint.
//!
//! Spins up the avatar route on a one-shot hyper server backed by a
//! sqlite::memory database, a tempdir-rooted `public` storage disk, and
//! the framework's real `SessionMiddleware` + `AuthMiddleware` stack.
//! Sessions are seeded directly through `DatabaseSessionDriver` and
//! handed to the client as an AES-256-GCM encrypted cookie produced
//! with the same key that the running middleware uses (the test
//! installs the process-wide `Crypt` key at setup; the middleware
//! refuses to run without one per codex review finding #1).
//!
//! The tests cover three branches:
//! - happy path: PNG bytes + caption → 200, file persisted on disk.
//! - validator: PDF bytes through the `Image` validator → 422.
//! - middleware: missing session cookie → 401 from `AuthMiddleware`.
//!
//! Why a process-wide mutex? `Storage`, `App::singleton` (DB +
//! UserProvider), and the route table all live in process-global state.
//! Running `#[tokio::test]` cases in parallel would clobber each other's
//! disks/DB/registrations. The framework's `Storage::fake()` already
//! takes a global lock; we layer a single `Mutex<()>` on top so the
//! entire per-test setup happens under one serialised critical section.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use sea_orm_migration::MigratorTrait;
use suprnova::http::cookie::Cookie;
use suprnova::session::driver::database::DatabaseSessionDriver;
use suprnova::session::{generate_csrf_token, generate_session_id, SessionData, SessionStore};
use suprnova::{
    bind, group, handle_request, post, AuthMiddleware, EncryptionKey, MiddlewareRegistry, Router,
    SessionConfig, SessionMiddleware, Storage, UserProvider,
};
use tokio::sync::Mutex;

use app::controllers;
use app::migrations::Migrator;
use app::models::users::User;
use app::providers::DatabaseUserProvider;

/// Process-wide serialisation lock. Holding it across the entire test
/// body keeps `App::singleton` (DB, user provider), the global storage
/// registry, and the framework's auth/session state from being mutated
/// by a sibling test mid-request.
///
/// `tokio::sync::Mutex` (rather than `std::sync::Mutex`) so the guard
/// is safe to hold across the many `.await` points in `setup_app` and
/// the request lifecycle — clippy's `await_holding_lock` lint correctly
/// flags `std::sync::Mutex` here, and tokio's mutex is designed for
/// exactly this use case.
static TEST_LOCK: Mutex<()> = Mutex::const_new(());

/// Test harness handle returned by `setup_app`. Owns the resources the
/// test depends on for the duration of the critical section:
///
/// - `_lock`: the process-wide test mutex (dropped last, releases all
///   other tests waiting to run).
/// - `_storage_guard`: the `Storage::fake()` guard which resets the
///   global disk registry on drop.
/// - `_tempdir`: the tempdir backing the `public` disk; deleted on
///   drop so we don't accumulate per-test scratch space.
/// - `addr`: socket address of the one-shot hyper server.
/// - `session_store`: cloneable driver used to seed sessions before
///   the request goes out.
struct TestApp {
    addr: SocketAddr,
    session_store: Arc<DatabaseSessionDriver>,
    storage_root: std::path::PathBuf,
    _tempdir: tempfile::TempDir,
    _storage_guard: suprnova::filesystem::testing::StorageFakeGuard,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

/// Minimal router exposing just the avatar upload endpoint, gated by
/// the framework's session-backed `AuthMiddleware`. Built inline because
/// `routes! { ... }` emits a `pub fn register()` definition, which only
/// works at module scope. Mirrors the structure the production route
/// uses in `app/src/routes.rs`.
fn build_router() -> Router {
    let router = Router::new();
    group!("/users", {
        post!("/avatar", controllers::avatar_upload::upload),
    })
    .middleware(AuthMiddleware::new())
    .register(router)
}

/// Build the per-test world: tempdir → public disk, sqlite::memory DB,
/// migrations, UserProvider, SessionMiddleware, hyper server.
///
/// Returns once the listener is bound and the accept loop is spawned.
/// The caller drops the `TestApp` to release the global lock.
async fn setup_app() -> TestApp {
    let lock = TEST_LOCK.lock().await;

    // Install the process-wide encryption key once. `Crypt::init` uses
    // `OnceLock` so this is idempotent across tests — the first test
    // in this binary supplies a fresh key; later tests in this binary
    // share it. The `SessionMiddleware` requires `Crypt` to be
    // initialised (codex review finding #1 / fail-closed boot path);
    // without this seed, every test would 500.
    suprnova::Crypt::init(EncryptionKey::generate());

    // Storage: install the test guard (resets the registry + serialises
    // against other `Storage::fake()` callers), then re-register `public`
    // pointed at a tempdir scoped to this test only.
    let storage_guard = Storage::fake();
    let tempdir = tempfile::tempdir().expect("create tempdir for public disk");
    let storage_root = tempdir.path().to_path_buf();
    Storage::register_fs("public", &storage_root).expect("register tempdir public disk");
    std::fs::create_dir_all(storage_root.join("avatars"))
        .expect("create avatars subdir under tempdir public root");

    // Database: in-memory SQLite, run migrations, register through the
    // framework's `DB` so `Auth::user_as` and `DatabaseSessionDriver`
    // see the same connection.
    let conn = sea_orm::Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite::memory:");
    Migrator::up(&conn, None)
        .await
        .expect("run migrations against sqlite::memory:");
    suprnova::App::singleton(suprnova::DbConnection::from_raw(conn));

    // User provider — needed by `Auth::user()` → `Auth::user_as::<User>()`.
    bind!(dyn UserProvider, DatabaseUserProvider);

    // SessionMiddleware with a custom store so we can both seed sessions
    // and let the middleware read them on the same connection. The
    // `secure(false)` mirrors how local dev would set `SESSION_SECURE=false`
    // — the hyper test client doesn't enforce the `Secure` attribute, so
    // this is a parity choice rather than a functional requirement.
    let session_config = SessionConfig::default().secure(false);
    let session_store: Arc<DatabaseSessionDriver> =
        Arc::new(DatabaseSessionDriver::new(session_config.lifetime));
    let session_middleware =
        SessionMiddleware::with_store(session_config, session_store.clone());

    let router = Arc::new(build_router());
    let middleware = Arc::new(MiddlewareRegistry::new().append(session_middleware));

    // One-shot hyper server. Each test issues a single HTTP round-trip
    // post-setup; the budget of 4 gives headroom for a retry or for
    // hyper's connection-pool semantics without leaving the accept loop
    // open indefinitely after the test body returns.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        for _ in 0..4 {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let io = TokioIo::new(stream);
            let router = router.clone();
            let middleware = middleware.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req: hyper::Request<Incoming>| {
                    let router = router.clone();
                    let middleware = middleware.clone();
                    async move {
                        Ok::<_, Infallible>(
                            handle_request(router, middleware, req).await,
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });

    TestApp {
        addr,
        session_store,
        storage_root,
        _tempdir: tempdir,
        _storage_guard: storage_guard,
        _lock: lock,
    }
}

/// Create a real user row, then seed a session row pointing at it.
/// Returns the AES-256-GCM encrypted cookie value that the caller
/// drops into a `suprnova_session=<encrypted-value>` cookie. The
/// `SessionMiddleware` decrypts inbound cookies via `Crypt` (which
/// the test seeded in `setup_app`); the plaintext fallback was
/// removed in codex review finding #1.
async fn seed_session_for_new_user(app: &TestApp) -> (User, String) {
    let user = User::create()
        .insert()
        .await
        .expect("insert seed user");
    let session_id = generate_session_id();
    let mut session = SessionData::new(session_id.clone(), generate_csrf_token());
    session.user_id = Some(user.id.to_string());
    session.dirty = true;
    app.session_store
        .write(&session)
        .await
        .expect("write seed session");
    // Encrypt the session id so the middleware's inbound-cookie
    // decryption path accepts it. We bypass the `Cookie` builder and
    // grab just the encrypted wire value (which is what would land in
    // an HTTP `Cookie` header from a real browser).
    let encrypted = Cookie::encrypted("suprnova_session", &session_id)
        .expect("Crypt installed at setup_app")
        .value()
        .to_string();
    (user, encrypted)
}

/// Build a multipart body from `(name, filename?, bytes)` parts.
/// Mirrors the helper used in framework integration tests.
fn build_multipart_body(boundary: &str, fields: &[(&str, Option<&str>, &[u8])]) -> Bytes {
    let mut body = Vec::new();
    for (name, file_name, bytes) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match file_name {
            Some(fname) => body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{fname}\"\r\n\
                     Content-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            ),
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            ),
        }
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Bytes::from(body)
}

/// 8-byte PNG signature + IHDR chunk. `infer::get` recognises this as
/// `image/png` so the `Image` validator accepts it.
fn tiny_png() -> Vec<u8> {
    let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x0D]);
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&[0; 13]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes
}

/// Send a POST to the avatar route. `session_cookie` is the raw cookie
/// value (no `suprnova_session=` prefix) or `None` for an unauth probe.
async fn post_avatar(
    addr: SocketAddr,
    body: Bytes,
    boundary: &str,
    session_cookie: Option<&str>,
) -> (hyper::http::StatusCode, hyper::HeaderMap, Bytes) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = hyper::Request::builder()
        .method("POST")
        .uri("/users/avatar")
        .header("Host", "localhost")
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header("Content-Length", body.len());
    if let Some(cookie) = session_cookie {
        builder = builder.header("Cookie", format!("suprnova_session={cookie}"));
    }
    let req = builder.body(Full::new(body)).unwrap();

    // Send the request with a generous timeout — the test server runs
    // on the same runtime, but a hang on a logic bug should surface as
    // a test failure rather than a CI watchdog kill.
    let resp = tokio::time::timeout(Duration::from_secs(10), sender.send_request(req))
        .await
        .expect("send_request did not complete within timeout")
        .expect("hyper send_request");
    let (parts, body) = resp.into_parts();
    let collected = body.collect().await.unwrap();
    (parts.status, parts.headers, collected.to_bytes())
}

#[tokio::test]
async fn avatar_upload_stores_file_on_public_disk() {
    let app = setup_app().await;
    let (user, session_cookie) = seed_session_for_new_user(&app).await;

    let png = tiny_png();
    let body = build_multipart_body(
        "x",
        &[
            ("avatar", Some("me.png"), &png),
            ("caption", None, b"Hello world"),
        ],
    );

    let (status, _headers, body) =
        post_avatar(app.addr, body, "x", Some(&session_cookie)).await;
    assert_eq!(status.as_u16(), 200, "upload should succeed");

    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("response should be JSON-parseable");
    let stored_at = json["stored_at"].as_str().expect("stored_at present");
    assert_eq!(
        stored_at,
        format!("avatars/{}.png", user.id),
        "path should be derived from user.id + sanitised extension"
    );
    assert_eq!(json["caption"], "Hello world");

    let on_disk = std::fs::read(app.storage_root.join(stored_at)).expect("file persisted");
    assert!(
        on_disk.starts_with(&png[..8]),
        "PNG signature should round-trip to disk"
    );
}

#[tokio::test]
async fn avatar_upload_rejects_non_image() {
    let app = setup_app().await;
    let (_user, session_cookie) = seed_session_for_new_user(&app).await;

    let pdf = b"%PDF-1.4 lorem ipsum dolor sit amet".to_vec();
    let body = build_multipart_body("x", &[("avatar", Some("not.pdf"), &pdf)]);

    let (status, _headers, _body) =
        post_avatar(app.addr, body, "x", Some(&session_cookie)).await;
    assert_eq!(
        status.as_u16(),
        422,
        "Image validator must reject non-image bytes with 422"
    );
}

#[tokio::test]
async fn avatar_upload_requires_authentication() {
    let app = setup_app().await;
    // No session seeded — the request goes out without a cookie at all.

    let png = tiny_png();
    let body = build_multipart_body("x", &[("avatar", Some("me.png"), &png)]);

    let (status, _headers, _body) = post_avatar(app.addr, body, "x", None).await;
    assert_eq!(
        status.as_u16(),
        401,
        "unauthenticated upload should be rejected by AuthMiddleware"
    );
}

