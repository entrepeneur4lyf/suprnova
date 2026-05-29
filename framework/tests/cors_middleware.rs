//! Integration tests for `CorsMiddleware`, installed as GLOBAL middleware.
//!
//! These prove the end-to-end behavior, including the server change that
//! runs the global middleware chain on unrouted requests so an OPTIONS
//! preflight (which never matches a route) still reaches CORS.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::http::text;
use suprnova::{CorsConfig, CorsMiddleware, MiddlewareRegistry, Router, handle_request};

/// Spawn a test server with `registry` as the global middleware set.
async fn spawn_server(
    router: impl Into<Router>,
    registry: MiddlewareRegistry,
    accepts: usize,
) -> SocketAddr {
    let router = Arc::new(router.into());
    let middleware = Arc::new(registry);

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

/// Send a request and return `(status, lowercased response headers, body)`.
async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, HashMap<String, String>, String) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Length", "0");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let req = builder.body(Full::new(Bytes::new())).unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");

    let (parts, body) = resp.into_parts();
    let status = parts.status.as_u16();
    let header_map = parts
        .headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_lowercase(),
                v.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let bytes = body.collect().await.unwrap().to_bytes();
    (
        status,
        header_map,
        String::from_utf8_lossy(&bytes).to_string(),
    )
}

/// Build a registry with CORS installed globally for `https://app.example`,
/// credentials on, 600s preflight cache.
fn cors_registry() -> MiddlewareRegistry {
    MiddlewareRegistry::new().append(CorsMiddleware::new(
        CorsConfig::allow_origins(["https://app.example"])
            .allow_credentials(true)
            .max_age(Duration::from_secs(600)),
    ))
}

fn router() -> Router {
    Router::new()
        .get("/api/data", |_req| async { text("data") })
        .into()
}

/// THE server-change proof: an OPTIONS preflight to a path with NO route
/// must still be answered by global CORS with 204 + headers, not a bare 404.
/// Without the unmatched-route middleware change this would 404.
#[tokio::test]
async fn preflight_to_unrouted_path_returns_204_not_404() {
    let addr = spawn_server(router(), cors_registry(), 3).await;
    let (status, headers, _body) = request(
        addr,
        "OPTIONS",
        "/no/such/route",
        &[
            ("Origin", "https://app.example"),
            ("Access-Control-Request-Method", "POST"),
            ("Access-Control-Request-Headers", "content-type"),
        ],
    )
    .await;

    assert_eq!(status, 204, "preflight must be answered 204, not 404");
    assert_eq!(
        headers
            .get("access-control-allow-origin")
            .map(String::as_str),
        Some("https://app.example")
    );
    assert!(
        headers
            .get("access-control-allow-methods")
            .map(|m| m.contains("POST"))
            .unwrap_or(false),
        "preflight must advertise allowed methods"
    );
    assert_eq!(
        headers
            .get("access-control-allow-credentials")
            .map(String::as_str),
        Some("true")
    );
    assert!(headers.contains_key("vary"));
}

/// An actual cross-origin request from an allowed origin is decorated with
/// CORS headers and the real handler still runs.
#[tokio::test]
async fn actual_request_from_allowed_origin_is_decorated() {
    let addr = spawn_server(router(), cors_registry(), 3).await;
    let (status, headers, body) = request(
        addr,
        "GET",
        "/api/data",
        &[("Origin", "https://app.example")],
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body, "data", "the real handler must still run");
    assert_eq!(
        headers
            .get("access-control-allow-origin")
            .map(String::as_str),
        Some("https://app.example")
    );
    assert_eq!(
        headers
            .get("access-control-allow-credentials")
            .map(String::as_str),
        Some("true")
    );
    assert_eq!(headers.get("vary").map(String::as_str), Some("Origin"));
}

/// A disallowed origin gets the real response but NO `Access-Control-Allow-
/// Origin`, so the browser blocks the cross-origin read.
#[tokio::test]
async fn disallowed_origin_gets_no_allow_origin_header() {
    let addr = spawn_server(router(), cors_registry(), 3).await;
    let (status, headers, body) = request(
        addr,
        "GET",
        "/api/data",
        &[("Origin", "https://evil.example")],
    )
    .await;

    assert_eq!(
        status, 200,
        "the handler still runs — CORS is a browser-side gate"
    );
    assert_eq!(body, "data");
    assert!(
        !headers.contains_key("access-control-allow-origin"),
        "a disallowed origin must NOT receive Access-Control-Allow-Origin"
    );
}

/// A same-origin request (no Origin header) is left completely untouched.
#[tokio::test]
async fn request_without_origin_is_untouched() {
    let addr = spawn_server(router(), cors_registry(), 3).await;
    let (status, headers, body) = request(addr, "GET", "/api/data", &[]).await;

    assert_eq!(status, 200);
    assert_eq!(body, "data");
    assert!(!headers.contains_key("access-control-allow-origin"));
    assert!(!headers.contains_key("vary"));
}

/// The unmatched-route middleware change must not break ordinary 404s: a
/// plain GET to an unrouted path still returns the 404.
#[tokio::test]
async fn unrouted_get_still_returns_404() {
    let addr = spawn_server(router(), cors_registry(), 3).await;
    let (status, _headers, body) = request(addr, "GET", "/no/such/route", &[]).await;

    assert_eq!(status, 404);
    assert_eq!(body, "404 Not Found");
}

/// CORS with `paths(["api/*"])` set: a cross-origin request to `/api/data`
/// is decorated; a cross-origin request to `/web/data` (out-of-scope) is
/// NOT decorated, even though the origin would otherwise be allowed.
#[tokio::test]
async fn paths_scoping_restricts_cors_to_matching_routes() {
    let registry = MiddlewareRegistry::new().append(CorsMiddleware::new(
        CorsConfig::allow_origins(["https://app.example"]).paths(["api/*"]),
    ));
    let router: Router = Router::new()
        .get("/api/data", |_req| async { text("api") })
        .get("/web/data", |_req| async { text("web") })
        .into();
    let addr = spawn_server(router, registry, 4).await;

    let (status, headers, body) = request(
        addr,
        "GET",
        "/api/data",
        &[("Origin", "https://app.example")],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, "api");
    assert_eq!(
        headers
            .get("access-control-allow-origin")
            .map(String::as_str),
        Some("https://app.example"),
        "in-scope path must get CORS headers"
    );

    let (status, headers, body) = request(
        addr,
        "GET",
        "/web/data",
        &[("Origin", "https://app.example")],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, "web");
    assert!(
        !headers.contains_key("access-control-allow-origin"),
        "out-of-scope path must NOT get CORS headers"
    );
}

/// A preflight to a `paths`-out-of-scope route is NOT short-circuited by
/// the CORS middleware — it falls through to the (missing) handler and
/// becomes a 404. This proves `paths` correctly gates BOTH preflight
/// handling and actual-response decoration.
#[tokio::test]
async fn paths_scoping_lets_out_of_scope_preflight_fall_through() {
    let registry = MiddlewareRegistry::new().append(CorsMiddleware::new(
        CorsConfig::allow_origins(["https://app.example"]).paths(["api/*"]),
    ));
    let addr = spawn_server(router(), registry, 2).await;

    let (status, headers, _body) = request(
        addr,
        "OPTIONS",
        "/web/no-route",
        &[
            ("Origin", "https://app.example"),
            ("Access-Control-Request-Method", "POST"),
        ],
    )
    .await;
    assert_eq!(status, 404);
    assert!(!headers.contains_key("access-control-allow-origin"));
}

/// A `skip_when` predicate that fires for `X-Internal` headers makes the
/// middleware forward the request directly, with no CORS decoration —
/// even though the origin would otherwise be allowed.
#[tokio::test]
async fn skip_when_predicate_short_circuits_cors() {
    let registry = MiddlewareRegistry::new().append(CorsMiddleware::new(
        CorsConfig::allow_origins(["https://app.example"])
            .skip_when(|req| req.header("X-Internal").is_some()),
    ));
    let addr = spawn_server(router(), registry, 2).await;

    let (status, headers, body) = request(
        addr,
        "GET",
        "/api/data",
        &[("Origin", "https://app.example"), ("X-Internal", "yes")],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body, "data");
    assert!(
        !headers.contains_key("access-control-allow-origin"),
        "skip_when must prevent CORS decoration"
    );
}

/// Regex `allow_origin_patterns` allows dynamic subdomains. A pattern
/// like `https://*.staging.example` lets any matching origin through
/// while still rejecting non-matching origins.
#[tokio::test]
async fn allow_origin_patterns_match_dynamic_subdomain() {
    let registry = MiddlewareRegistry::new().append(CorsMiddleware::new(
        CorsConfig::allow_origins(Vec::<String>::new())
            .allow_origin_patterns([r"https://[a-z0-9-]+\.staging\.example"]),
    ));
    let addr = spawn_server(router(), registry, 3).await;

    let (status, headers, _body) = request(
        addr,
        "GET",
        "/api/data",
        &[("Origin", "https://pr-42.staging.example")],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        headers
            .get("access-control-allow-origin")
            .map(String::as_str),
        Some("https://pr-42.staging.example"),
        "pattern-matched origin must be echoed back"
    );

    let (status, headers, _body) = request(
        addr,
        "GET",
        "/api/data",
        &[("Origin", "https://evil.example")],
    )
    .await;
    assert_eq!(status, 200);
    assert!(
        !headers.contains_key("access-control-allow-origin"),
        "non-matching origin must NOT be echoed back"
    );
}
