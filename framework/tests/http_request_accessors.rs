//! Laravel-13 parity tests for the `Request` accessor surface.
//!
//! These verify the URL / host / content / header / route helpers
//! added to [`suprnova::Request`]. Each test constructs a request via
//! a hyper service so the body type is `Incoming` (the only shape
//! `Request::new` accepts) and then asserts the accessor result.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use std::convert::Infallible;
use std::net::IpAddr;
use suprnova::Request;
use suprnova::http::TrustedProxiesConfig;
use suprnova::routing::register_route_name;

/// Trusted-proxy config that lists `127.0.0.1` — paired with a
/// `with_peer_addr(127.0.0.1)` it makes the proxy-aware accessors
/// honour the configured `X-Forwarded-*` / `X-Real-IP` headers, which
/// is the deployment shape these legacy tests exercise.
fn trust_loopback() -> TrustedProxiesConfig {
    TrustedProxiesConfig::with_ips([IpAddr::from([127, 0, 0, 1])])
}

/// Construct a `suprnova::Request` from a hyper `Request<Full<Bytes>>`
/// by piping it through a hyper service so the body becomes
/// `Incoming`. The returned Request can then be probed by any
/// accessor.
async fn build_request(builder: hyper::http::request::Builder, body: &str) -> suprnova::Request {
    use hyper_util::rt::TokioIo;
    use tokio::net::{TcpListener, TcpStream};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body_bytes = body.to_string();
    let (req_tx, req_rx) = tokio::sync::oneshot::channel::<suprnova::Request>();
    let req_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(req_tx)));

    let req_tx_for_svc = req_tx.clone();
    let server_task = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let svc = service_fn(move |hyper_req: hyper::Request<Incoming>| {
                let tx = req_tx_for_svc.clone();
                async move {
                    let req = Request::new(hyper_req);
                    if let Some(s) = tx.lock().unwrap().take() {
                        let _ = s.send(req);
                    }
                    Ok::<hyper::Response<Full<Bytes>>, Infallible>(
                        hyper::Response::builder()
                            .status(200)
                            .body(Full::new(Bytes::from_static(b"")))
                            .unwrap(),
                    )
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), svc)
                .await;
        }
    });

    let client_task = tokio::spawn(async move {
        let stream = TcpStream::connect(addr).await.unwrap();
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let req = builder.body(Full::new(Bytes::from(body_bytes))).unwrap();
        let resp = sender.send_request(req).await.unwrap();
        let _ = resp.into_body().collect().await;
    });

    let received = req_rx.await.unwrap();
    let _ = client_task.await;
    let _ = server_task.await;
    received
}

#[tokio::test]
async fn bearer_token_extracts_simple_token() {
    let req = build_request(
        hyper::Request::builder()
            .method("GET")
            .uri("/api/users")
            .header("Authorization", "Bearer secret-token-123"),
        "",
    )
    .await;
    assert_eq!(req.bearer_token().as_deref(), Some("secret-token-123"));
}

#[tokio::test]
async fn bearer_token_handles_case_insensitive_prefix() {
    let req = build_request(
        hyper::Request::builder()
            .method("GET")
            .uri("/api/users")
            .header("Authorization", "bearer abc"),
        "",
    )
    .await;
    assert_eq!(req.bearer_token().as_deref(), Some("abc"));
}

#[tokio::test]
async fn bearer_token_returns_none_when_absent() {
    let req = build_request(
        hyper::Request::builder().method("GET").uri("/api/users"),
        "",
    )
    .await;
    assert!(req.bearer_token().is_none());
}

#[tokio::test]
async fn bearer_token_strips_trailing_comma_list() {
    // Laravel's bearerToken truncates at the comma if multiple
    // credentials are listed.
    let req = build_request(
        hyper::Request::builder()
            .method("GET")
            .uri("/")
            .header("Authorization", "Bearer foo, scheme=other"),
        "",
    )
    .await;
    assert_eq!(req.bearer_token().as_deref(), Some("foo"));
}

#[tokio::test]
async fn ajax_pjax_and_prefetch_detection() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Requested-With", "XMLHttpRequest"),
        "",
    )
    .await;
    assert!(req.ajax());
    assert!(!req.pjax());
    assert!(!req.prefetch());

    let req = build_request(
        hyper::Request::builder().uri("/").header("X-PJAX", "true"),
        "",
    )
    .await;
    assert!(req.pjax());

    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("Purpose", "prefetch"),
        "",
    )
    .await;
    assert!(req.prefetch());
}

#[tokio::test]
async fn is_method_case_insensitive() {
    let req = build_request(hyper::Request::builder().method("POST").uri("/"), "").await;
    assert!(req.is_method("POST"));
    assert!(req.is_method("post"));
    assert!(!req.is_method("GET"));
}

#[tokio::test]
async fn has_header_returns_true_when_present() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Custom-Header", "value"),
        "",
    )
    .await;
    assert!(req.has_header("X-Custom-Header"));
    assert!(req.has_header("x-custom-header"));
    assert!(!req.has_header("X-Missing"));
}

#[tokio::test]
async fn secure_detects_x_forwarded_proto_https() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Forwarded-Proto", "https"),
        "",
    )
    .await
    .with_peer_addr(IpAddr::from([127, 0, 0, 1]))
    .with_trusted_proxies(trust_loopback());
    assert!(req.secure());
    assert_eq!(req.scheme(), "https");
}

#[tokio::test]
async fn secure_detects_x_forwarded_ssl_on() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Forwarded-Ssl", "on"),
        "",
    )
    .await
    .with_peer_addr(IpAddr::from([127, 0, 0, 1]))
    .with_trusted_proxies(trust_loopback());
    assert!(req.secure());
}

#[tokio::test]
async fn secure_returns_false_without_proxy_headers() {
    let req = build_request(hyper::Request::builder().uri("/"), "").await;
    assert!(!req.secure());
    assert_eq!(req.scheme(), "http");
}

#[tokio::test]
async fn secure_ignores_x_forwarded_proto_from_untrusted_peer() {
    // Peer is NOT in the (empty) allowlist — header must be ignored,
    // matching the fail-safe default that protects deployments behind
    // an untrusted edge.
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Forwarded-Proto", "https"),
        "",
    )
    .await
    .with_peer_addr(IpAddr::from([127, 0, 0, 1]));
    assert!(
        !req.secure(),
        "untrusted peer must not lift the secure flag"
    );
    assert_eq!(req.scheme(), "http");
}

#[tokio::test]
async fn ip_reads_x_forwarded_for_first_hop() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Forwarded-For", "203.0.113.5, 10.0.0.1, 10.0.0.2"),
        "",
    )
    .await
    .with_peer_addr(IpAddr::from([127, 0, 0, 1]))
    .with_trusted_proxies(trust_loopback());
    assert_eq!(req.ip().as_deref(), Some("203.0.113.5"));
}

#[tokio::test]
async fn ip_falls_back_to_x_real_ip() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Real-IP", "198.51.100.7"),
        "",
    )
    .await
    .with_peer_addr(IpAddr::from([127, 0, 0, 1]))
    .with_trusted_proxies(trust_loopback());
    assert_eq!(req.ip().as_deref(), Some("198.51.100.7"));
}

#[tokio::test]
async fn ip_falls_back_to_peer_addr() {
    let req = build_request(hyper::Request::builder().uri("/"), "")
        .await
        .with_peer_addr(IpAddr::from([127, 0, 0, 1]));
    assert_eq!(req.ip().as_deref(), Some("127.0.0.1"));
}

#[tokio::test]
async fn ip_ignores_x_forwarded_for_from_untrusted_peer() {
    // Same shape as the trusted-XFF test but without a configured
    // allowlist — the framework MUST return the TCP peer, not the
    // attacker-controlled XFF.
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Forwarded-For", "203.0.113.5"),
        "",
    )
    .await
    .with_peer_addr(IpAddr::from([198, 51, 100, 2]));
    assert_eq!(
        req.ip().as_deref(),
        Some("198.51.100.2"),
        "untrusted peer must not propagate spoofed XFF"
    );
}

#[tokio::test]
async fn ips_chains_proxy_headers_and_peer_addr() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Forwarded-For", "203.0.113.5, 10.0.0.1"),
        "",
    )
    .await
    .with_peer_addr(IpAddr::from([127, 0, 0, 1]))
    .with_trusted_proxies(trust_loopback());
    let chain = req.ips();
    assert_eq!(chain, vec!["203.0.113.5", "10.0.0.1", "127.0.0.1"]);
}

#[tokio::test]
async fn ips_with_untrusted_peer_omits_x_forwarded_for_entries() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Forwarded-For", "203.0.113.5, 10.0.0.1"),
        "",
    )
    .await
    .with_peer_addr(IpAddr::from([127, 0, 0, 1]));
    let chain = req.ips();
    assert_eq!(
        chain,
        vec!["127.0.0.1"],
        "spoofed XFF hops must not appear in the chain when the peer isn't a trusted proxy"
    );
}

#[tokio::test]
async fn host_reads_x_forwarded_host_then_host() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Forwarded-Host", "api.example.com:8443"),
        "",
    )
    .await
    .with_peer_addr(IpAddr::from([127, 0, 0, 1]))
    .with_trusted_proxies(trust_loopback());
    // Port stripped, host returned bare.
    assert_eq!(req.host().as_deref(), Some("api.example.com"));
    assert_eq!(req.port(), Some(8443));
}

#[tokio::test]
async fn segments_and_segment_1_based() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/users/42/posts")
            .method("GET"),
        "",
    )
    .await;
    assert_eq!(req.segments(), vec!["users", "42", "posts"]);
    assert_eq!(req.segment(1, None).as_deref(), Some("users"));
    assert_eq!(req.segment(2, None).as_deref(), Some("42"));
    assert_eq!(
        req.segment(99, Some("fallback")).as_deref(),
        Some("fallback")
    );
}

#[tokio::test]
async fn decoded_path_resolves_percent_escapes() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/space%20here/x")
            .method("GET"),
        "",
    )
    .await;
    assert_eq!(req.decoded_path(), "/space here/x");
}

#[tokio::test]
async fn url_and_full_url_with_proxy_host() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/dashboard?tab=metrics&page=2")
            .header("Host", "app.example.com")
            .header("X-Forwarded-Proto", "https"),
        "",
    )
    .await
    .with_peer_addr(IpAddr::from([127, 0, 0, 1]))
    .with_trusted_proxies(trust_loopback());
    assert_eq!(req.url(), "https://app.example.com/dashboard");
    assert_eq!(
        req.full_url(),
        "https://app.example.com/dashboard?tab=metrics&page=2"
    );
}

#[tokio::test]
async fn full_url_with_query_overrides_and_appends() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/x?a=1&b=2")
            .header("Host", "app.example.com")
            .header("X-Forwarded-Proto", "https"),
        "",
    )
    .await
    .with_peer_addr(IpAddr::from([127, 0, 0, 1]))
    .with_trusted_proxies(trust_loopback());
    let merged = req.full_url_with_query(&[("c", "3"), ("d", "4")]);
    assert!(merged.contains("c=3"));
    assert!(merged.contains("d=4"));
    assert!(merged.starts_with("https://app.example.com/x?"));
}

#[tokio::test]
async fn full_url_without_query_removes_keys() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/x?a=1&b=2&c=3")
            .header("Host", "app.example.com")
            .header("X-Forwarded-Proto", "https"),
        "",
    )
    .await;
    let pruned = req.full_url_without_query(&["a", "c"]);
    assert!(pruned.contains("b=2"));
    assert!(!pruned.contains("a=1"));
    assert!(!pruned.contains("c=3"));
}

#[tokio::test]
async fn query_param_returns_value_or_none() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/x?id=42&page=7")
            .method("GET"),
        "",
    )
    .await;
    assert_eq!(req.query_param("id").as_deref(), Some("42"));
    assert_eq!(req.query_param("page").as_deref(), Some("7"));
    assert!(req.query_param("missing").is_none());
    assert!(req.has_query("page"));
    assert!(!req.has_query("missing"));
}

#[tokio::test]
async fn query_params_returns_map() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/x?a=1&b=hello%20world&c=")
            .method("GET"),
        "",
    )
    .await;
    let map = req.query_params();
    assert_eq!(map.get("a").map(|s| s.as_str()), Some("1"));
    assert_eq!(map.get("b").map(|s| s.as_str()), Some("hello world"));
    assert_eq!(map.get("c").map(|s| s.as_str()), Some(""));
}

#[tokio::test]
async fn is_matches_wildcard_pattern() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/admin/users/list")
            .method("GET"),
        "",
    )
    .await;
    assert!(req.is(&["admin/*"]));
    assert!(req.is(&["admin/users/*"]));
    assert!(!req.is(&["api/*"]));
    assert!(req.is(&["api/*", "admin/*"]));
}

#[tokio::test]
async fn route_is_consults_matched_pattern_name() {
    register_route_name("_test_users_show_routeis", "/users/show/{id}");
    let req = build_request(
        hyper::Request::builder().uri("/users/show/7").method("GET"),
        "",
    )
    .await
    .with_route_pattern("/users/show/{id}");
    assert!(req.route_is(&["_test_users_show_routeis"]));
    assert!(req.route_is(&["_test_*"]));
    assert!(!req.route_is(&["unrelated.*"]));
}

#[tokio::test]
async fn route_pattern_and_name_resolution() {
    register_route_name("_test_pat_name", "/widgets/{id}");
    let req = build_request(
        hyper::Request::builder().uri("/widgets/123").method("GET"),
        "",
    )
    .await
    .with_route_pattern("/widgets/{id}");
    assert_eq!(req.route_pattern(), Some("/widgets/{id}"));
    assert_eq!(req.route_name().as_deref(), Some("_test_pat_name"));
}

#[tokio::test]
async fn is_json_detects_content_type() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("Content-Type", "application/json"),
        "",
    )
    .await;
    assert!(req.is_json());

    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("Content-Type", "application/vnd.api+json"),
        "",
    )
    .await;
    assert!(req.is_json());

    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("Content-Type", "text/html"),
        "",
    )
    .await;
    assert!(!req.is_json());
}

#[tokio::test]
async fn accepts_returns_true_for_listed_content_type() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("Accept", "application/json, text/html;q=0.9"),
        "",
    )
    .await;
    assert!(req.accepts(&["application/json"]));
    assert!(req.accepts(&["text/html"]));
    assert!(req.accepts_json());
    assert!(req.accepts_html());
    assert!(!req.accepts(&["application/xml"]));
}

#[tokio::test]
async fn accepts_falls_through_with_no_accept_header() {
    let req = build_request(hyper::Request::builder().uri("/"), "").await;
    assert!(req.accepts(&["application/json"]));
    assert!(req.accepts_any_content_type());
}

#[tokio::test]
async fn prefers_picks_first_match_in_q_order() {
    let req = build_request(
        hyper::Request::builder().uri("/").header(
            "Accept",
            "text/html;q=0.5, application/json;q=0.9, */*;q=0.1",
        ),
        "",
    )
    .await;
    assert_eq!(
        req.prefers(&["application/json", "text/html"]).as_deref(),
        Some("application/json")
    );
}

#[tokio::test]
async fn expects_json_when_ajax_and_no_accept_specific() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("X-Requested-With", "XMLHttpRequest"),
        "",
    )
    .await;
    assert!(req.expects_json());
}

#[tokio::test]
async fn wants_json_top_preference() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("Accept", "application/json, text/html"),
        "",
    )
    .await;
    assert!(req.wants_json());

    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("Accept", "text/html, application/json"),
        "",
    )
    .await;
    assert!(!req.wants_json());
}

#[tokio::test]
async fn user_agent_returns_header_value() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/")
            .header("User-Agent", "suprnova-test/1.0"),
        "",
    )
    .await;
    assert_eq!(req.user_agent(), Some("suprnova-test/1.0"));
}

/// Compile-only test pinning the hyper aliasing surface at the crate
/// root. `Request::method()` returns `&Method`, `uri()` returns `&Uri`,
/// `headers()` returns `&HeaderMap`, and the streaming body type is
/// `Incoming` — all hyper-owned. Re-exporting them under `suprnova::*`
/// means consumers never need to add `hyper` to their Cargo.toml just
/// to name those types in a handler signature.
#[test]
fn hyper_types_reachable_via_crate_root() {
    // Named imports: these will fail to resolve if the re-exports go
    // away.
    use suprnova::{HeaderMap, Method, RequestBodyStream, StatusCode, Uri};

    // Documented escape hatch: the full `hyper` module is reachable as
    // `suprnova::hyper::*` for anything the explicit aliases don't
    // cover.
    use suprnova::hyper;

    // Construct one of each to prove the aliases name the right hyper
    // types. The compiler does the work; runtime asserts just keep the
    // bindings live.
    let _: Method = Method::GET;
    let _: StatusCode = StatusCode::OK;
    let _: Uri = "/".parse().unwrap();
    let _: HeaderMap = HeaderMap::new();

    // `RequestBodyStream` is `hyper::body::Incoming`. We can't
    // construct one directly (no public ctor) but we CAN prove the
    // alias and the escape-hatch path resolve to the same type via a
    // type-checked transmute trick: a fn pointer with two
    // ostensibly-different parameter types only compiles if rustc sees
    // them as the same type.
    fn _alias_matches_escape_hatch(a: RequestBodyStream) -> hyper::body::Incoming {
        a
    }
}
