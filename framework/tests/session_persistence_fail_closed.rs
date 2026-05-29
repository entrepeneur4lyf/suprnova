//! Production gate #375 — session persistence failures fail closed.
//!
//! When the session store's `write` fails AND the session was mutated
//! this request (dirty bit set by a login, logout, CSRF rotation, flash,
//! remember-me hydration, ...), `SessionMiddleware` must return 500
//! rather than returning the handler's success response with a session
//! cookie for state the store never recorded. Otherwise a "successful"
//! login that never persisted would hand the client a cookie whose id has
//! no backing row — the next request loads an empty session and the
//! mutation silently vanishes.
//!
//! Conversely, a NON-dirty session (the write was only a `last_activity`
//! touch) must still pass through: a transient store outage shouldn't
//! turn every read-only request into a 500.
//!
//! Drives a real `Request` through `SessionMiddleware::handle` using the
//! same inlined duplex-pipe + hyper harness as
//! `remember_me.rs::middleware_hydrates_session_from_remember_cookie`
//! (the shared `common.rs` helper is module-private and can't cross
//! test-binary boundaries).

use async_trait::async_trait;
use std::sync::Arc;
use suprnova::session::{SessionConfig, SessionData, SessionMiddleware, SessionStore};
use suprnova::{Crypt, EncryptionKey, FrameworkError};

/// `Crypt` is a process-global; install a key exactly once so
/// `SessionMiddleware::handle` doesn't bail at its top-of-fn guard
/// (which also returns 500 — see the body assertion in the dirty test
/// for why that distinction matters).
fn ensure_crypt() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        Crypt::init(EncryptionKey::generate());
    });
}

/// Session store whose `write` always fails — simulates a backing-store
/// outage. `read` returns `Ok(None)` so the middleware mints a fresh
/// session; whether that session ends up dirty is controlled entirely by
/// the test's handler.
struct FailingStore;

#[async_trait]
impl SessionStore for FailingStore {
    async fn read(&self, _id: &str) -> Result<Option<SessionData>, FrameworkError> {
        Ok(None)
    }
    async fn write(&self, _session: &SessionData) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal("simulated session store outage"))
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

/// Build a real `Request` (GET /) by feeding raw HTTP bytes through a
/// hyper service over an in-memory duplex pipe. `Request::new` only
/// accepts a `hyper::Request<Incoming>`, and `Incoming` bodies can't be
/// synthesized directly, so we let hyper parse a real request and hand it
/// back over a oneshot.
async fn get_request() -> suprnova::Request {
    use bytes::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use suprnova::Request;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::oneshot;

    let http_bytes = b"GET / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n".to_vec();

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
                // Park forever: we only need hyper to parse the request
                // and hand it over; the response it would produce is
                // irrelevant because the middleware drives the handler.
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
    // `cookie_secure(false)` so we don't have to think about HTTPS here.
    SessionConfig {
        cookie_secure: false,
        ..SessionConfig::default()
    }
}

/// A mutated session whose write fails must surface as 500 — and the body
/// must come from the fail-closed write path, not the top-of-handle Crypt
/// guard (which is also a 500).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dirty_session_write_failure_fails_closed_500() {
    use http_body_util::BodyExt;
    use suprnova::middleware::{Middleware, Next};

    ensure_crypt();

    // Handler mutates the session (a login) -> dirty, so the failed write
    // MUST fail closed.
    let next: Next = Arc::new(move |_req| {
        Box::pin(async move {
            suprnova::session::set_auth_user("fail-closed-user");
            Ok(suprnova::HttpResponse::text("ok"))
        })
    });

    let middleware = SessionMiddleware::with_store(test_config(), Arc::new(FailingStore));
    let response = middleware.handle(get_request().await, next).await;

    let err = match response {
        Err(r) => r,
        Ok(_) => panic!("a mutated session whose write failed must fail closed, not return Ok"),
    };
    assert_eq!(
        err.status_code(),
        500,
        "dirty-session write failure must surface as 500"
    );

    // Body must be the persistence-failure message. This pins WHICH 500
    // fired: the Crypt guard at the top of `handle` also returns 500, but
    // with a different body, and `ensure_crypt` means we never hit it.
    let body = err
        .into_hyper()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let body = String::from_utf8_lossy(&body);
    assert!(
        body.contains("session persistence failed"),
        "the 500 must come from the fail-closed write path; got body: {body}"
    );
}

/// An unmodified session whose `last_activity` write fails must pass
/// through: the user-visible state is intact, and 500-ing every read-only
/// request during a transient store outage would be worse than the lost
/// activity-timestamp bump. (This passing also proves `Crypt` is
/// installed — otherwise the top-of-handle guard would 500 here too,
/// which confirms the sibling test's 500 is genuinely our branch.)
///
/// Note on POST: this test originally drove a GET request, but the
/// `SessionMiddleware` now writes `_previous.url` on successful HTML
/// GETs (Laravel's `StartSession::storeCurrentUrl` behaviour, which
/// powers `Redirect::back`). That makes every GET a "dirty" mutation
/// even when the handler doesn't touch the session. POST keeps the
/// original semantics: no `_previous.url` write, so an untouched session
/// stays clean — letting the read-only-store-outage assertion below
/// still pin the clean-write branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_session_write_failure_passes_through() {
    use suprnova::middleware::{Middleware, Next};

    ensure_crypt();

    // Handler does NOT touch the session. The middleware still attempts a
    // last_activity write, which fails — but nothing was mutated.
    let next: Next =
        Arc::new(move |_req| Box::pin(async move { Ok(suprnova::HttpResponse::text("ok")) }));

    let middleware = SessionMiddleware::with_store(test_config(), Arc::new(FailingStore));
    let response = middleware.handle(post_request().await, next).await;

    let ok = match response {
        Ok(r) => r,
        Err(r) => panic!(
            "an unmodified session whose last-activity write failed must pass through; got {}",
            r.status_code()
        ),
    };
    assert_eq!(
        ok.status_code(),
        200,
        "clean-session write failure must not change the handler's success status"
    );
}

/// POST-equivalent of [`get_request`]. The middleware skips its
/// `_previous.url` write on non-GET verbs, so a POST-driven test exercises
/// the "unmodified session" branch even after the GET-side
/// previous-URL write landed.
async fn post_request() -> suprnova::Request {
    use bytes::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use suprnova::Request;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::oneshot;

    let http_bytes = b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n".to_vec();

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
                Ok::<_, Infallible>(hyper::Response::new(http_body_util::Full::new(
                    Bytes::from_static(b""),
                )))
            }
        });
        let _ = http1::Builder::new()
            .serve_connection(TokioIo::new(server_io), svc)
            .await;
    });

    let mut client = client_io;
    client.write_all(&http_bytes).await.unwrap();
    drop(client);
    req_rx.await.expect("request to be captured")
}
