//! End-to-end tests for the real SeaORM-backed Post model and the
//! `/api/posts*` endpoints (codex review finding #17).
//!
//! The dogfood `app/src/models/posts.rs` previously returned hardcoded
//! data — these tests prove the model now hits a real database and
//! that the controllers behave correctly under `SessionAuthMiddleware`
//! + `PostPolicy` Gate authorization.
//!
//! Why a process-wide mutex? The framework's `App::singleton`,
//! `bind!`, and inventory-registered policy gates all live in
//! process-global state. Running `#[tokio::test]` cases in parallel
//! within this binary would clobber each other's DB and user provider
//! registrations. We mirror the lock pattern from
//! `app/tests/avatar_upload_e2e.rs`.

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
    attrs, bind, delete, get, group, handle_request, post,
    AuthMiddleware as SessionAuthMiddleware, EncryptionKey, MiddlewareRegistry, Model, Router,
    SessionConfig, SessionMiddleware, UserProvider,
};
use tokio::sync::Mutex;

use app::controllers;
use app::migrations::Migrator;
use app::models::posts::Post;
use app::models::users::User;
use app::providers::DatabaseUserProvider;

/// Serialises every test in this file. See module docs above for why.
static TEST_LOCK: Mutex<()> = Mutex::const_new(());

/// Per-test world handle.
struct TestApp {
    addr: SocketAddr,
    session_store: Arc<DatabaseSessionDriver>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

/// Stand up the router that production wires in `app/src/routes.rs`.
/// `routes! { ... }` only works at module scope, so we mirror the
/// inline `group!` / `get!` / `post!` shape here.
///
/// `GET /api/posts` is the unauthenticated public listing; the
/// auth-gated group reuses the same path string for `POST /` and
/// `GET /{id}`. The middleware map is keyed by `(method, path)`, so
/// the public GET does not inherit the auth middleware bound to the
/// POST on the same path.
fn build_router() -> Router {
    let router = Router::new();
    let router = get!("/api/posts", controllers::posts::index).register(router);
    // Auth-gated POST + GET-by-id + DELETE share the `/api/posts`
    // prefix; the group binds SessionAuthMiddleware to each route
    // under its own `(method, path)` key.
    group!("/api/posts", {
        get!("/{id}", controllers::posts::show),
        post!("/", controllers::posts::store),
        delete!("/{id}", controllers::admin::delete_post),
    })
    .middleware(SessionAuthMiddleware::new())
    .register(router)
}

/// Build the per-test world: in-memory SQLite + migrations, register
/// `DB` + `UserProvider`, install `Crypt`, spin up a one-shot hyper
/// server. Initialisation registers the `PostPolicy` gates at link
/// time via `inventory::submit!` so they're available the moment the
/// test runs without explicit init.
async fn setup_app() -> TestApp {
    let lock = TEST_LOCK.lock().await;

    // Process-wide encryption key. `Crypt::init` uses `OnceLock` so
    // this is idempotent across tests in the same binary; the first
    // test seeds, the rest no-op.
    suprnova::Crypt::init(EncryptionKey::generate());

    // Fresh sqlite::memory DB + migrations. `App::singleton` allows
    // re-registration, so each test in this binary swaps in its own
    // connection. Migrations include the new posts table from finding
    // #17.
    let conn = sea_orm::Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite::memory:");
    Migrator::up(&conn, None)
        .await
        .expect("run migrations against sqlite::memory:");
    suprnova::App::singleton(suprnova::DbConnection::from_raw(conn));

    bind!(dyn UserProvider, DatabaseUserProvider);
    // Initialise policy gates registered via `#[policy(User, Post)]`.
    // Idempotent: subsequent calls are no-ops.
    suprnova::authorization::init_policies();

    let session_config = SessionConfig::default().secure(false);
    let session_store: Arc<DatabaseSessionDriver> =
        Arc::new(DatabaseSessionDriver::new(session_config.lifetime));
    let session_middleware =
        SessionMiddleware::with_store(session_config, session_store.clone());

    let router = Arc::new(build_router());
    let middleware = Arc::new(MiddlewareRegistry::new().append(session_middleware));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        // Six accepts: index list + show + create + delete + auth-rejection
        // probes give five-ish slots; allow one retry margin so a flaky
        // hyper connect doesn't strand a test.
        for _ in 0..8 {
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
                        Ok::<_, Infallible>(handle_request(router, middleware, req).await)
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
        _lock: lock,
    }
}

/// Insert a user row, then seed a session row pointing at it. Returns
/// the encrypted cookie value the test drops into a
/// `suprnova_session=<value>` Cookie header.
async fn seed_session_for_new_user(app: &TestApp) -> (User, String) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let user = User::create(attrs! {
        name: "Posts E2E User",
        email: format!("posts-{seq}@example.suprnova.app"),
        password: "hashed-by-test",
    })
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
    let encrypted = Cookie::encrypted("suprnova_session", &session_id)
        .expect("Crypt installed at setup_app")
        .value()
        .to_string();
    (user, encrypted)
}

/// Issue one HTTP round-trip against the test server. `cookie` is the
/// raw encrypted session value (no `suprnova_session=` prefix) or
/// `None` for an unauthenticated probe.
async fn send_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    cookie: Option<&str>,
) -> (hyper::http::StatusCode, Bytes) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let body_bytes = match body {
        Some(v) => Bytes::from(serde_json::to_vec(v).unwrap()),
        None => Bytes::new(),
    };

    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "localhost");
    if body.is_some() {
        builder = builder
            .header("Content-Type", "application/json")
            .header("Content-Length", body_bytes.len());
    } else {
        builder = builder.header("Content-Length", "0");
    }
    if let Some(c) = cookie {
        builder = builder.header("Cookie", format!("suprnova_session={c}"));
    }
    let req = builder.body(Full::new(body_bytes)).unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(10), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");
    let (parts, body) = resp.into_parts();
    let collected = body.collect().await.unwrap().to_bytes();
    (parts.status, collected)
}

#[tokio::test]
async fn create_post_inserts_real_row_owned_by_session_user() {
    let app = setup_app().await;
    let (user, cookie) = seed_session_for_new_user(&app).await;

    let (status, body) = send_request(
        app.addr,
        "POST",
        "/api/posts",
        Some(&serde_json::json!({
            "title": "Hello, Suprnova!",
            "body": "Real DB-backed post.",
            "is_public": true,
        })),
        Some(&cookie),
    )
    .await;

    assert_eq!(
        status.as_u16(),
        201,
        "POST /api/posts must return 201 Created; got body {:?}",
        std::str::from_utf8(&body).unwrap_or("<binary>")
    );

    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("response is JSON");
    assert!(json["id"].as_i64().unwrap() > 0, "id assigned by SQL");
    assert_eq!(
        json["author_id"].as_i64().unwrap(),
        user.id,
        "author_id comes from the session, not the request body"
    );
    assert_eq!(json["title"], "Hello, Suprnova!");
    assert_eq!(json["is_public"], true);

    // Verify the row really is in the DB (not just echoed from the
    // response). `Post::find_by_id` round-trips through the real
    // model code path we shipped in finding #17.
    let id = json["id"].as_i64().unwrap();
    let persisted = Post::find_by_id(id).await.unwrap().expect("row exists");
    assert_eq!(persisted.author_id, user.id);
    assert_eq!(persisted.title, "Hello, Suprnova!");
}

#[tokio::test]
async fn list_public_posts_returns_only_public_rows() {
    let app = setup_app().await;
    let (_user, cookie) = seed_session_for_new_user(&app).await;

    // Create one public + one private post.
    let (s1, _) = send_request(
        app.addr,
        "POST",
        "/api/posts",
        Some(&serde_json::json!({
            "title": "Public hello",
            "body": "Visible to everyone.",
            "is_public": true,
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(s1.as_u16(), 201);

    let (s2, _) = send_request(
        app.addr,
        "POST",
        "/api/posts",
        Some(&serde_json::json!({
            "title": "Private draft",
            "body": "Secret stuff.",
            "is_public": false,
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(s2.as_u16(), 201);

    // GET /api/posts is unauthenticated. Sibling POST /api/posts is
    // auth-gated by SessionAuthMiddleware on the same path string;
    // because the middleware map is keyed by `(method, path)`, the
    // public GET does not inherit the POST's middleware.
    let (status, body) = send_request(app.addr, "GET", "/api/posts", None, None).await;
    assert_eq!(status.as_u16(), 200);
    let json: serde_json::Value =
        serde_json::from_slice(&body).expect("response is JSON");
    let posts = json["posts"].as_array().expect("posts array");

    assert!(
        posts.iter().any(|p| p["title"] == "Public hello"),
        "public post must appear in listing"
    );
    assert!(
        posts.iter().all(|p| p["title"] != "Private draft"),
        "private post must NOT appear in unauthenticated listing"
    );
}

#[tokio::test]
async fn create_post_requires_authentication() {
    let app = setup_app().await;

    let (status, body) = send_request(
        app.addr,
        "POST",
        "/api/posts",
        Some(&serde_json::json!({
            "title": "Sneaky",
            "body": "Should be blocked.",
            "is_public": true,
        })),
        // No cookie → SessionAuthMiddleware returns 401.
        None,
    )
    .await;

    assert_eq!(
        status.as_u16(),
        401,
        "unauthenticated create must return 401; got body {:?}",
        std::str::from_utf8(&body).unwrap_or("<binary>")
    );
}

#[tokio::test]
async fn show_post_runs_view_gate_and_rejects_private() {
    let app = setup_app().await;
    let (_user, cookie) = seed_session_for_new_user(&app).await;

    // Create one private post.
    let (s_create, body) = send_request(
        app.addr,
        "POST",
        "/api/posts",
        Some(&serde_json::json!({
            "title": "Private",
            "body": "Hidden.",
            "is_public": false,
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(s_create.as_u16(), 201);
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = created["id"].as_i64().unwrap();

    // Hit GET /api/posts/{id} — PostPolicy::view returns false on
    // !is_public, so Gate::authorize emits 403.
    let (status, _) = send_request(
        app.addr,
        "GET",
        &format!("/api/posts/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status.as_u16(),
        403,
        "private post must be blocked by view-post gate"
    );
}

#[tokio::test]
async fn delete_post_runs_delete_gate_and_removes_row() {
    let app = setup_app().await;
    let (_user, cookie) = seed_session_for_new_user(&app).await;

    // Create a post.
    let (s_create, body) = send_request(
        app.addr,
        "POST",
        "/api/posts",
        Some(&serde_json::json!({
            "title": "To be deleted",
            "body": "rm -rf.",
            "is_public": true,
        })),
        Some(&cookie),
    )
    .await;
    assert_eq!(s_create.as_u16(), 201);
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = created["id"].as_i64().unwrap();

    // Delete it — owner passes the `delete-post` gate.
    let (s_del, _) = send_request(
        app.addr,
        "DELETE",
        &format!("/api/posts/{id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(s_del.as_u16(), 200, "owner can delete their post");

    // Verify the row really vanished from the DB.
    let after = Post::find_by_id(id).await.unwrap();
    assert!(after.is_none(), "post row must be deleted from DB");
}
