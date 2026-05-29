//! End-to-end test: `GET /config` must NEVER expose credentials or
//! connection strings.
//!
//! The example endpoint at `controllers::config_example::show` exists
//! to demonstrate "how to read config from a handler" — a tempting
//! pattern to copy into real apps. This test pins the scrubbing
//! contract so the demo can't accidentally regress to dumping the
//! database URL, SMTP host, password, or any other secret that would
//! leak through `Config::register(DatabaseConfig::from_env())` in a
//! production-shaped registration.
//!
//! We deliberately register *sentinel* configs with distinctive
//! pretend-credentials so the "no secret in body" assertion has real
//! teeth: the default `from_env()` fallback uses a sqlite file URL
//! with no password, which would weakly "pass" the assertion just by
//! virtue of having nothing to leak. Registering
//! `postgres://sn_user:sn_pass@sn-host:5432/sn_db` makes regressions
//! visible.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::{
    Config, DatabaseConfig, MiddlewareRegistry, Request, Response, get, handle_request, routes,
};

use app::config::MailConfig;
use app::controllers::config_example;

/// Wrapper handler so the test binary can mount the controller without
/// pulling in the rest of `app::routes`. Forwards the request straight
/// through.
async fn config_show(req: Request) -> Response {
    config_example::show(req).await
}

routes! {
    get!("/config", config_show),
}

/// Distinctive sentinels that MUST NOT appear anywhere in the scrubbed
/// response body. Keeping them as `const` so the assertion error
/// messages show exactly which sentinel leaked.
const SENTINEL_DB_USER: &str = "sn_user";
const SENTINEL_DB_PASSWORD: &str = "sn_pass";
const SENTINEL_DB_HOST: &str = "sn-host";
const SENTINEL_MAIL_HOST: &str = "sn-mail-host.example";
const SENTINEL_MAIL_PASSWORD: &str = "sn-mail-secret";
const SENTINEL_MAIL_USERNAME: &str = "sn-mail-user";
const SENTINEL_MAIL_FROM: &str = "from@sn-mail-host.example";

/// Register sentinel configs through the framework's process-wide
/// `Config` repository. `Config::register` is last-write-wins
/// (HashMap insert) so this overwrites any earlier bootstrap.
fn register_sentinel_configs() {
    let db = DatabaseConfig::builder()
        .url(format!(
            "postgres://{SENTINEL_DB_USER}:{SENTINEL_DB_PASSWORD}@{SENTINEL_DB_HOST}:5432/sn_db"
        ))
        .max_connections(7)
        .min_connections(2)
        .connect_timeout(11)
        .logging(true)
        .build();
    Config::register(db);

    let mail = MailConfig {
        driver: "smtp".to_string(),
        host: SENTINEL_MAIL_HOST.to_string(),
        port: 2525,
        username: SENTINEL_MAIL_USERNAME.to_string(),
        password: SENTINEL_MAIL_PASSWORD.to_string(),
        from_address: SENTINEL_MAIL_FROM.to_string(),
        from_name: "Suprnova Sentinel".to_string(),
    };
    Config::register(mail);
}

/// Spawn the one-shot hyper server in front of the single `/config`
/// route. One accept per request keeps the test crisp.
async fn spawn(accepts: usize) -> SocketAddr {
    register_sentinel_configs();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Arc::new(register());
    let middleware = Arc::new(MiddlewareRegistry::new());

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

/// Drive a single `GET /config` round-trip and return (status, body bytes).
async fn fetch_config(addr: SocketAddr) -> (hyper::http::StatusCode, Bytes) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("GET")
        .uri("/config")
        .header("Host", "localhost")
        .body(Empty::<Bytes>::new())
        .unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");
    let (parts, body) = resp.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (parts.status, bytes)
}

#[tokio::test]
async fn config_route_scrubs_secrets_and_connection_strings() {
    let addr = spawn(1).await;
    let (status, body) = fetch_config(addr).await;
    assert_eq!(status, hyper::http::StatusCode::OK, "GET /config must 200");

    let body_str = String::from_utf8_lossy(&body).to_string();
    let lower = body_str.to_lowercase();

    // Section 1 — the URL must not leak. The driver-scheme prefixes are
    // the structural fingerprint of a leak; flagging them here catches
    // *any* connection-string copy regardless of which credentials
    // happen to be in env.
    for fingerprint in [
        "postgres://",
        "postgresql://",
        "mysql://",
        "sqlite://",
        "sqlite:",
        "@", // user@host or addr@host in any URL or email — none of those belong here
    ] {
        assert!(
            !body_str.contains(fingerprint),
            "response body must not contain `{fingerprint}` (would leak connection string \
             or credential locator): body=`{body_str}`",
        );
    }

    // Section 2 — the specific sentinels we registered must not appear.
    // If any of these surfaces we know exactly which field leaked.
    for (label, sentinel) in [
        ("db user", SENTINEL_DB_USER),
        ("db password", SENTINEL_DB_PASSWORD),
        ("db host", SENTINEL_DB_HOST),
        ("mail host", SENTINEL_MAIL_HOST),
        ("mail password", SENTINEL_MAIL_PASSWORD),
        ("mail username", SENTINEL_MAIL_USERNAME),
        ("mail from-address", SENTINEL_MAIL_FROM),
    ] {
        assert!(
            !body_str.contains(sentinel),
            "response body must not contain sentinel `{sentinel}` ({label}): body=`{body_str}`",
        );
    }

    // Section 3 — credential field names must not appear as JSON keys.
    // We assert on the *quoted* form so a future controller that names
    // a field "password" or "secret" trips this even if the value is
    // sanitised. Bare-substring checks would false-positive on legit
    // text (e.g. a "non-secret" status string), so we anchor on the
    // JSON-key delimiter pair.
    for key in ["\"password\"", "\"secret\"", "\"token\"", "\"username\""] {
        let key_lower = key.to_lowercase();
        assert!(
            !lower.contains(&key_lower),
            "response body must not contain credential JSON key `{key}` \
             (case-insensitive): body=`{body_str}`",
        );
    }

    // Section 4 — positive assertions. Prove the scrub kept the
    // educational payload alive. The driver type (debug-formatted
    // `DatabaseType` variant) and the mail driver name should both
    // surface so users learn the right pattern, not the empty one.
    assert!(
        body_str.contains("Postgres"),
        "response body should expose the database TYPE (driver family) — \
         got body=`{body_str}`",
    );
    assert!(
        body_str.contains("\"smtp\""),
        "response body should expose the mail driver name — got body=`{body_str}`",
    );
    assert!(
        body_str.contains("safe fields only"),
        "response body should carry the scrub-principle marker so the demo \
         documents itself — got body=`{body_str}`",
    );
}
