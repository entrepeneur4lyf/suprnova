//! End-to-end test for Phase 13's request-path wiring.
//!
//! Spins up a one-shot hyper server with `FeatureMiddleware` installed
//! in front of a tiny handler that calls
//! `NEW_CHECKOUT_FLOW.is_enabled()` and writes `ON` or `OFF` into the
//! response body. Drives three round-trips:
//!
//! 1. Baseline — no row in `features`, `is_enabled` returns the
//!    compile-time default (`false`), handler writes `OFF`.
//! 2. After `admin::upsert("new-checkout-flow", "", true, ...)`, the
//!    next request observes the new value — `ON`.
//! 3. After `admin::delete`, falls back to default — `OFF`.
//!
//! This is the Phase 13 audit-fix #1 test: the framework's composition
//! tests (`framework/tests/features.rs`) prove the FeatureSync chain
//! propagates correctly in isolation. This test proves the chain is
//! still doing its job when wrapped inside an actual `Server` /
//! `Router` / `MiddlewareRegistry` / `Request<Incoming>` plumbing — the
//! gap the advisor flagged at the close of Phase 13.
//!
//! Why a process-wide mutex: featureflag's global default evaluator is
//! `OnceLock`-backed, the App container is process-wide, and our
//! `INSTALLED` tracker is a static `AtomicBool`. Two parallel tests
//! exercising any of them would clobber each other. The avatar-upload
//! test in this directory uses the same TEST_LOCK pattern for the same
//! reason.

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
use suprnova::container::App;
use suprnova::features::sync::FeatureSync;
use suprnova::features::{
    CachedEvaluator, CompositeFeatureSync, DatabaseEvaluator, FeatureMiddleware, admin,
    install_evaluator,
};
use suprnova::http::text;
use suprnova::{DbConnection, MiddlewareRegistry, Request, Response, get, handle_request, routes};
use tokio::sync::Mutex;

use app::features::NEW_CHECKOUT_FLOW;
use app::migrations::Migrator;

/// Same serialisation guard pattern the avatar test uses — without it,
/// parallel tests in this binary trample each other's process-global
/// state (App container, featureflag default, INSTALLED tracker).
static TEST_LOCK: Mutex<()> = Mutex::const_new(());

/// Tiny handler the test drives — calls `NEW_CHECKOUT_FLOW.is_enabled()`
/// and reports the answer in plain text. Bypasses the home controller's
/// Inertia / ExampleAction dependencies on purpose — what we want to
/// test is the middleware → handler → `is_enabled` path, not the
/// surrounding plumbing.
async fn flag_probe(_req: Request) -> Response {
    let body = if NEW_CHECKOUT_FLOW.is_enabled() {
        "ON"
    } else {
        "OFF"
    };
    text(body)
}

routes! {
    get!("/flag", flag_probe),
}

/// Resources the test holds for the duration of its critical section.
/// Drop order matters: the listener guard is released last so the
/// hyper accept loop can return cleanly.
struct TestApp {
    addr: SocketAddr,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

/// Build the per-test world. Uses sqlite::memory for hermeticity and
/// binds everything through the App / TestContainer plumbing exactly
/// the way `bootstrap_database_cached` would do it in production — but
/// without using the helper itself, because the helper sets the
/// featureflag global default which is process-wide and we'd then
/// fight other tests in this binary. install_evaluator handles the
/// already-set case gracefully (see `framework/src/features/bootstrap.rs`),
/// so the first call wins for the binary's lifetime, and subsequent
/// runs are valid no-ops that still update the per-test FeatureSync
/// binding.
async fn setup_app() -> TestApp {
    let lock = TEST_LOCK.lock().await;

    // Database — sqlite::memory + app migrations (which include
    // CreateFeaturesTable from Phase 13 T7's migration registration).
    let conn = sea_orm::Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite::memory:");
    Migrator::up(&conn, None)
        .await
        .expect("run migrations against sqlite::memory:");
    App::singleton(DbConnection::from_raw(conn));

    // Build the evaluator chain. Mirrors what
    // `bootstrap_database_cached` does, but uses `App::bind` for the
    // FeatureSync wiring (single-threaded test, process-wide app
    // container is exactly what `notify()` reads).
    let database = Arc::new(DatabaseEvaluator::new().await.expect("DatabaseEvaluator"));
    let cached = Arc::new(CachedEvaluator::new(
        database.clone() as Arc<dyn suprnova::features::Evaluator + Send + Sync>,
        Duration::from_secs(60),
    ));
    let composite = Arc::new(CompositeFeatureSync::new(
        vec![database.clone() as Arc<dyn FeatureSync>],
        vec![cached.clone() as Arc<dyn FeatureSync>],
    ));
    App::bind::<dyn FeatureSync>(composite);

    // Install the cached evaluator as featureflag's global default so
    // `context!` macro's `on_new_context` hook fires. After the first
    // test in this binary, subsequent calls warn-and-no-op (advisor
    // flag from the install_evaluator fix); that's fine — the same
    // evaluator already routes through the snapshot we just rebuilt.
    install_evaluator(cached.clone());

    // Router + middleware stack. FeatureMiddleware sits in the global
    // chain so every request opens a context — same shape as the
    // production bootstrap.rs registers it.
    let router = Arc::new(register());
    let middleware = Arc::new(MiddlewareRegistry::new().append(FeatureMiddleware::new()));

    // One-shot hyper server. Three accepts: baseline (off), post-upsert
    // (on), post-delete (off). Bump if the test grows more probes.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        for _ in 0..3 {
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
                    async move { Ok::<_, Infallible>(handle_request(router, middleware, req).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });

    TestApp { addr, _lock: lock }
}

/// GET /flag and return the response body as a String.
async fn get_flag_body(addr: SocketAddr) -> String {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("GET")
        .uri("/flag")
        .header("Host", "localhost")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");
    let (_parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn flag_propagates_through_request_path_on_upsert_and_delete() {
    let app = setup_app().await;

    // Baseline: no row → compile-time default (`false`) wins → handler
    // writes "OFF". If FeatureMiddleware were broken (didn't open a
    // context, or InContext's poll-reentry lost the scope across
    // handler awaits), `is_enabled()` would either panic on an
    // uninitialised context lookup or return some stale state — either
    // way, "OFF" wouldn't be the cleanest answer to assert against.
    assert_eq!(
        get_flag_body(app.addr).await,
        "OFF",
        "baseline: no row in features table → compile-time default false → handler reports OFF",
    );

    // Toggle on via admin. R1's FeatureSync fan-out invalidates the
    // cache + reloads the DB snapshot before this call returns.
    admin::upsert("new-checkout-flow", "", true, None, None)
        .await
        .expect("admin::upsert");

    assert_eq!(
        get_flag_body(app.addr).await,
        "ON",
        "after admin::upsert: middleware + handler must observe the new value without manual reload \
         and without waiting for the TTL — this is the kill-switch contract Phase 13 R1 ships",
    );

    // Delete: row gone → compile-time default takes over again.
    admin::delete("new-checkout-flow", "", None)
        .await
        .expect("admin::delete");

    assert_eq!(
        get_flag_body(app.addr).await,
        "OFF",
        "after admin::delete: snapshot drops the row, cache invalidates, fallback to compile-time \
         default reports OFF",
    );
}
