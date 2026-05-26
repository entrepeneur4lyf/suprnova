//! Phase 11 — `BruteForce` facade integration tests.
//!
//! Shape mirrors `framework/tests/password_reset.rs` (shared tokio
//! runtime + one-time `init_torii` via `Lazy<()>`). See
//! `framework/tests/email_verify.rs`'s module docs for the reasoning
//! around the shared runtime and `#[serial]`.
//!
//! Brute-force tests do not touch `Mail::fake()`, but the torii
//! instance is still process-global. `#[serial]` keeps the shared
//! sqlite pool's runtime affinity stable across tests, and unique
//! per-test emails (`alice-bf@`, `bob-bf@`, …) keep the rows
//! addressable without inter-test interference.

use once_cell::sync::Lazy;
use serial_test::serial;
use tokio::runtime::Runtime;

use suprnova::auth_flows::BruteForce;
use suprnova::torii_integration::{ToriiConfig, init_torii};

/// One tokio runtime shared across every test in this file.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// One-time Torii initialisation shared across all tests.
static SETUP: Lazy<()> = Lazy::new(|| {
    RT.block_on(async {
        let config = ToriiConfig::sqlite_in_memory()
            .await
            .expect("sqlite in-memory connection")
            .apply_migrations(true);
        init_torii(config).await.expect("init_torii");
    });
});

#[test]
#[serial]
fn record_and_lockout_lifecycle() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // Register so the email exists; the brute-force service itself
        // does not require the user to exist, but having the row matches
        // the real flow.
        suprnova::Auth::password()
            .register("alice-bf@example.com", "longpassword123")
            .await
            .unwrap();

        // Fresh account: not locked.
        assert!(
            !BruteForce::is_locked("alice-bf@example.com").await.unwrap(),
            "freshly-registered account must not be locked"
        );

        // Drive the failed-attempt counter to the default threshold
        // (BruteForceProtectionConfig::default().max_failed_attempts == 5).
        // The 5th call records the attempt that crosses the threshold.
        for _ in 0..5 {
            BruteForce::record_failed_attempt("alice-bf@example.com", Some("203.0.113.7"))
                .await
                .unwrap();
        }

        assert!(
            BruteForce::is_locked("alice-bf@example.com").await.unwrap(),
            "account must be locked after 5 failed attempts (default threshold)"
        );

        let status = BruteForce::get_lockout_status("alice-bf@example.com")
            .await
            .unwrap();
        assert!(status.is_locked, "lockout_status.is_locked must be true");
        assert!(
            status.failed_attempts >= 5,
            "failed_attempts must be at least 5, got {}",
            status.failed_attempts
        );
        assert!(
            status.locked_until.is_some(),
            "locked_until must be populated while locked"
        );

        // First unlock: was previously locked → returns true.
        let was_locked = BruteForce::unlock_account("alice-bf@example.com")
            .await
            .unwrap();
        assert!(
            was_locked,
            "first unlock_account on a locked account must return true"
        );
        assert!(
            !BruteForce::is_locked("alice-bf@example.com").await.unwrap(),
            "account must not be locked after unlock_account"
        );

        // Second unlock: account is already unlocked → returns false.
        let was_locked_second = BruteForce::unlock_account("alice-bf@example.com")
            .await
            .unwrap();
        assert!(
            !was_locked_second,
            "second unlock_account on an already-unlocked account must return false"
        );
    });
}

#[test]
#[serial]
fn reset_attempts_clears_counter() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        suprnova::Auth::password()
            .register("bob-bf@example.com", "longpassword123")
            .await
            .unwrap();

        BruteForce::record_failed_attempt("bob-bf@example.com", None)
            .await
            .unwrap();
        BruteForce::record_failed_attempt("bob-bf@example.com", None)
            .await
            .unwrap();

        let status = BruteForce::get_lockout_status("bob-bf@example.com")
            .await
            .unwrap();
        assert!(
            status.failed_attempts >= 2,
            "after two failed attempts the counter must be >= 2, got {}",
            status.failed_attempts
        );
        assert!(
            !status.is_locked,
            "two attempts is below the default threshold (5); must not lock"
        );

        BruteForce::reset_attempts("bob-bf@example.com")
            .await
            .unwrap();

        let after = BruteForce::get_lockout_status("bob-bf@example.com")
            .await
            .unwrap();
        assert_eq!(
            after.failed_attempts, 0,
            "reset_attempts must zero the failed-attempt counter"
        );
        assert!(
            !after.is_locked,
            "reset_attempts must leave the account unlocked"
        );
    });
}

#[test]
#[serial]
fn get_lockout_status_for_unknown_email_returns_unlocked() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // The brute-force service computes lockout dynamically from
        // recent attempt rows. An email with no rows → unlocked, zero
        // attempts. No user row needed.
        let status = BruteForce::get_lockout_status("nobody-bf@example.com")
            .await
            .expect("get_lockout_status must handle missing emails gracefully");

        assert!(
            !status.is_locked,
            "unknown email must report is_locked = false"
        );
        assert_eq!(
            status.failed_attempts, 0,
            "unknown email must report failed_attempts = 0"
        );
        assert!(
            status.locked_until.is_none(),
            "unknown email must not have a locked_until timestamp"
        );
    });
}

#[test]
#[serial]
fn unlock_account_on_unknown_email_returns_false() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // Forced unlock on an email that has no attempt rows and isn't
        // locked must be a clean no-op returning false. This is the
        // T0 bool-fix contract: false means "wasn't locked", not an error.
        let was_locked = BruteForce::unlock_account("ghost-bf@example.com")
            .await
            .expect("unlock_account on unknown email must not error");
        assert!(
            !was_locked,
            "unlock_account on an unlocked / unknown email must return false"
        );
    });
}

// ============================================================================
// LoginThrottleMiddleware — HTTP-layer integration tests
// ============================================================================
//
// We exercise the middleware end-to-end through a real `Router` server,
// the same pattern used by `framework/tests/rate_limit_middleware.rs`.
//
// The email is extracted from a request header (`X-Login-Email`). Reading
// the request body would consume `Request`, so the middleware's email
// extractor closure is sync-over-`&Request` — header / query / route param
// are the available extraction surfaces. The login form's email is
// expected to be mirrored to a header by the framework's session /
// CSRF middleware in practice, or by the user-supplied extractor.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::auth_flows::LoginThrottleMiddleware;
use suprnova::http::text;
use suprnova::{MiddlewareRegistry, Router, handle_request};

/// Spawn a test HTTP/1.1 server bound to an ephemeral port, dispatch
/// up to `accepts` connections via `handle_request`, then exit.
async fn spawn_server(router: impl Into<Router>, accepts: usize) -> SocketAddr {
    let router = Arc::new(router.into());
    let middleware = Arc::new(MiddlewareRegistry::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral listener");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        for _ in 0..accepts {
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

    addr
}

/// POST /login with an optional `X-Login-Email` header. Returns
/// `(status, retry_after_header_value)`.
async fn post_login(addr: SocketAddr, email: Option<&str>) -> (u16, Option<String>) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = hyper::Request::builder()
        .method("POST")
        .uri("/login")
        .header("Host", "localhost")
        .header("Content-Length", "0");
    if let Some(e) = email {
        builder = builder.header("X-Login-Email", e);
    }
    let req = builder.body(Full::new(Bytes::new())).unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");

    let (parts, body) = resp.into_parts();
    let _ = body.collect().await.unwrap();
    let retry_after = parts
        .headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok().map(String::from));
    (parts.status.as_u16(), retry_after)
}

/// Build a `LoginThrottleMiddleware` that pulls the email from
/// the `X-Login-Email` header.
fn header_throttle() -> LoginThrottleMiddleware {
    LoginThrottleMiddleware::new(|req: &suprnova::Request| {
        req.header("X-Login-Email").map(|s| s.to_string())
    })
}

#[test]
#[serial]
fn middleware_passes_through_when_email_missing() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let router = Router::new()
            .post("/login", |_req| async { text("login-ok") })
            .middleware(header_throttle());

        let addr = spawn_server(router, 5).await;

        let (status, _) = post_login(addr, None).await;
        assert_eq!(
            status, 200,
            "absent X-Login-Email header must pass the request through"
        );
    });
}

#[test]
#[serial]
fn middleware_passes_through_when_account_not_locked() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        suprnova::Auth::password()
            .register("clara-bf@example.com", "longpassword123")
            .await
            .unwrap();

        // Fresh user — no failed attempts → not locked.
        let router = Router::new()
            .post("/login", |_req| async { text("login-ok") })
            .middleware(header_throttle());

        let addr = spawn_server(router, 5).await;

        let (status, _) = post_login(addr, Some("clara-bf@example.com")).await;
        assert_eq!(
            status, 200,
            "present email + not-locked account must pass through"
        );
    });
}

#[test]
#[serial]
fn middleware_429s_when_account_locked() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        suprnova::Auth::password()
            .register("dora-bf@example.com", "longpassword123")
            .await
            .unwrap();

        // Drive the account into the locked state.
        for _ in 0..5 {
            BruteForce::record_failed_attempt("dora-bf@example.com", None)
                .await
                .unwrap();
        }
        assert!(
            BruteForce::is_locked("dora-bf@example.com").await.unwrap(),
            "precondition: dora must be locked"
        );

        let router = Router::new()
            .post("/login", |_req| async { text("login-ok") })
            .middleware(header_throttle());

        let addr = spawn_server(router, 5).await;

        let (status, retry) = post_login(addr, Some("dora-bf@example.com")).await;
        assert_eq!(
            status, 429,
            "locked account must short-circuit with 429 before the handler runs"
        );
        let retry = retry.expect("429 must carry a Retry-After header");
        // Default lockout_period is 15 minutes — retry-after should be
        // a positive integer in seconds, near 900 but bounded by clock
        // drift between record-attempts and the middleware fetching
        // status. Accept any value in (0, 900].
        let secs: u64 = retry
            .parse()
            .expect("Retry-After header must be a u64 of seconds");
        assert!(
            secs > 0 && secs <= 900,
            "Retry-After must be a positive number of seconds, ≤ 900 (default lockout window); got {secs}"
        );
    });
}

#[test]
#[serial]
fn account_locked_fires_once_on_transition() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        use suprnova::auth_flows::AccountLocked;
        use suprnova::events::EventFacade;
        use suprnova::events::testing::dispatched_count;

        suprnova::Auth::password()
            .register("eve-bf@example.com", "longpassword123")
            .await
            .unwrap();

        let _guard = EventFacade::fake();

        // Threshold = 5. The 5th call crosses unlocked → locked; only
        // that one should fire AccountLocked. The 6th should not
        // re-fire because the account is already locked when we
        // observe state before recording.
        for _ in 0..6 {
            BruteForce::record_failed_attempt("eve-bf@example.com", None)
                .await
                .unwrap();
        }

        let fires = dispatched_count::<AccountLocked>(|e| e.email == "eve-bf@example.com");
        assert_eq!(
            fires, 1,
            "AccountLocked must fire exactly once on the unlocked→locked transition, got {fires}"
        );
    });
}
