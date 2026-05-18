use crate::cache::Cache;
use crate::config::{Config, ServerConfig};
use crate::container::App;
use crate::http::{HttpResponse, Request};
use crate::logging::{LogConfig, RequestIdMiddleware};
use crate::middleware::{into_boxed, Middleware, MiddlewareChain, MiddlewareRegistry};
use crate::routing::Router;
use crate::telemetry::{init_telemetry, OtelConfig};
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use std::collections::HashMap;

/// Alias for the body type the server returns into hyper. All
/// `HttpResponse` variants (static + streaming) collapse to this so
/// the service signature stays uniform.
type ServerBody = BoxBody<Bytes, Infallible>;

pub struct Server {
    router: Arc<Router>,
    middleware: MiddlewareRegistry,
    host: String,
    port: u16,
}

impl Server {
    pub fn new(router: impl Into<Router>) -> Self {
        Self {
            router: Arc::new(router.into()),
            middleware: MiddlewareRegistry::new(),
            host: "127.0.0.1".to_string(),
            port: 8000,
        }
    }

    /// Build a [`Server`] from process configuration.
    ///
    /// # Errors
    ///
    /// Returns a [`FrameworkError`] if the encryption key cannot be
    /// installed. Specifically:
    ///
    /// - `APP_ENV` resolves to a non-development environment (anything
    ///   other than local/development/testing) AND `APP_KEY` is unset
    ///   or empty. Production fails closed per codex review finding #1.
    /// - `APP_KEY` is set but malformed (wrong length, not base64).
    ///
    /// Local, development, and testing environments generate a
    /// transient dev key when `APP_KEY` is unset, so `cargo run` stays
    /// zero-config. A loud `tracing::warn!` is emitted in that case so
    /// the operator notices sessions won't persist across restarts.
    pub fn from_config(router: impl Into<Router>) -> Result<Self, crate::FrameworkError> {
        // Initialize the App container
        App::init();

        // Boot all auto-registered services from #[service(ConcreteType)]
        App::boot_services();

        // Install the process-wide encryption key.
        //
        // Production / staging / custom envs fail closed when APP_KEY
        // is missing — `resolve_boot_key` returns Err with an
        // actionable message that we propagate as the boot error.
        //
        // Local / development / testing fall through to a generated
        // transient key. We log a loud warn so the operator notices.
        //
        // The `is_initialized()` guard makes this idempotent in tests
        // (and embedders that call `from_config` more than once). On
        // re-entry, we keep whatever key is already installed.
        if !crate::crypto::Crypt::is_initialized() {
            let environment = Config::get::<crate::config::AppConfig>()
                .map(|c| c.environment)
                .unwrap_or_else(crate::config::Environment::detect);
            let app_key = std::env::var("APP_KEY").ok();
            let boot_key = crate::crypto::resolve_boot_key(&environment, app_key.as_deref())?;

            if boot_key.is_generated() {
                tracing::warn!(
                    environment = %environment,
                    "APP_KEY is not set — generated a transient development key. \
                     Sessions and cursors will reset on every restart. Set APP_KEY \
                     in your environment to persist them. This path is gated to \
                     local/development/testing; production fails closed."
                );
            }
            crate::crypto::Crypt::init(boot_key.into_key());
        }

        let config = Config::get::<ServerConfig>().unwrap_or_else(ServerConfig::from_env);
        Ok(Self {
            router: Arc::new(router.into()),
            // Pull global middleware registered via global_middleware! in bootstrap.rs
            middleware: MiddlewareRegistry::from_global(),
            host: config.host,
            port: config.port,
        })
    }

    /// Add global middleware (runs on every request)
    ///
    /// For route-specific middleware, use `.middleware(M)` on the route itself.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Server::from_config(router)?
    ///     .middleware(LoggingMiddleware)  // Global
    ///     .middleware(CorsMiddleware)     // Global
    ///     .run()
    ///     .await;
    /// ```
    pub fn middleware<M: Middleware + 'static>(mut self, middleware: M) -> Self {
        self.middleware = self.middleware.append(middleware);
        self
    }

    pub fn host(mut self, host: &str) -> Self {
        self.host = host.to_string();
        self
    }

    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Parse `self.host` as an `IpAddr` and combine with `self.port` into a
    /// [`SocketAddr`] suitable for `TcpListener::bind`.
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError::Internal`] if `self.host` is not a valid IP
    /// address literal. The message identifies the bad value and shows the
    /// expected format so misconfiguration surfaces during boot with an
    /// actionable diagnostic instead of an opaque process panic.
    ///
    /// IPv4 and IPv6 literals are both accepted (e.g. `127.0.0.1`, `::1`).
    /// Hostnames must be resolved by the caller before reaching this path;
    /// `Server::host()` accepts strings verbatim.
    fn get_addr(&self) -> Result<SocketAddr, crate::FrameworkError> {
        let ip: std::net::IpAddr = self.host.parse().map_err(|e| {
            crate::FrameworkError::internal(format!(
                "invalid server host '{}': {e}. Expected an IP literal such as '127.0.0.1' or '::1'.",
                self.host,
            ))
        })?;
        Ok(SocketAddr::new(ip, self.port))
    }

    pub async fn run(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Initialize the global tracing subscriber (and OTel pipelines
        // when the `otel` feature is enabled + an endpoint is set).
        // The guard owns the SDK providers and flushes them on Ctrl-C
        // or SIGTERM. Idempotent across calls.
        let guard = init_telemetry(LogConfig::from_env(), OtelConfig::from_env());

        // Register all #[policy] gates collected via inventory::submit!
        crate::authorization::init_policies();

        // Bootstrap cache (Redis with in-memory fallback)
        Cache::bootstrap().await;

        // Bootstrap queue and rate-limit drivers from env vars.
        // Defaults to in-memory when QUEUE_DRIVER / RATE_LIMIT_DRIVER are unset.
        crate::queue::bootstrap_from_env().await?;
        crate::rate_limit::bootstrap_from_env().await?;

        // Bootstrap the mail transport from MAIL_DRIVER. Defaults to the
        // `log` driver when the env var is unset.
        crate::mail::boot::bootstrap_from_env()?;

        let addr: SocketAddr = self.get_addr()?;
        let listener = TcpListener::bind(addr).await?;

        tracing::info!(%addr, "suprnova server listening");

        let router = self.router;
        let middleware = Arc::new(self.middleware);

        // Track in-flight connections so shutdown can drain them
        // before flushing OTel buffers. Each accepted connection is
        // spawned into this JoinSet rather than via bare tokio::spawn.
        let mut connections: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

        loop {
            tokio::select! {
                accept = listener.accept() => {
                    let (stream, _) = accept?;
                    let io = TokioIo::new(stream);
                    let router = router.clone();
                    let middleware = middleware.clone();

                    connections.spawn(async move {
                        let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                            let router = router.clone();
                            let middleware = middleware.clone();
                            async move {
                                Ok::<_, Infallible>(handle_request(router, middleware, req).await)
                            }
                        });

                        if let Err(err) = http1::Builder::new()
                            .serve_connection(io, service)
                            .with_upgrades()
                            .await
                        {
                            tracing::error!(?err, "error serving connection");
                        }
                    });
                }
                // Reap completed connections to keep the JoinSet small.
                // join_next() returns None if the set is empty — we treat
                // that as "stay parked" by branching only on Some.
                Some(_) = connections.join_next() => {}
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("shutdown signal received (Ctrl-C)");
                    break;
                }
                _ = wait_terminate() => {
                    tracing::info!("SIGTERM received");
                    break;
                }
            }
        }

        // Drain in-flight connections before flushing telemetry. Spans
        // and metrics emitted by these tasks need to land in the
        // batch processors BEFORE we call shutdown(). Bound the drain
        // window so a slow client can't block shutdown forever.
        tracing::info!(
            in_flight = connections.len(),
            "draining in-flight connections (max 10s)"
        );
        let drain_deadline = tokio::time::sleep(std::time::Duration::from_secs(10));
        tokio::pin!(drain_deadline);
        loop {
            tokio::select! {
                next = connections.join_next() => {
                    if next.is_none() {
                        break; // JoinSet empty — all drained
                    }
                }
                _ = &mut drain_deadline => {
                    tracing::warn!(
                        in_flight = connections.len(),
                        "drain deadline exceeded; abandoning remaining connections"
                    );
                    break;
                }
            }
        }

        // Flush buffered telemetry before returning. Safe to call when
        // OTel is disabled — guard just no-ops.
        guard.shutdown().await;
        Ok(())
    }
}

/// Wait for SIGTERM on Unix. On non-Unix platforms returns a future that
/// never resolves, so the `tokio::select!` arm stays parked.
#[cfg(unix)]
async fn wait_terminate() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut sig) => {
            sig.recv().await;
        }
        Err(err) => {
            tracing::warn!(?err, "failed to install SIGTERM handler; \
                Ctrl-C is still honored");
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(not(unix))]
async fn wait_terminate() {
    std::future::pending::<()>().await;
}

/// Serve a single inbound `hyper::Request<Incoming>` against the
/// supplied `router` and `middleware_registry`, returning the
/// framework's `hyper::Response<BoxBody<Bytes, Infallible>>` exactly
/// the way `Server::run` does internally.
///
/// Intended for tests and embedders that want to wire the framework
/// into their own hyper service loop. `Server::run` is the production
/// path; this is the in-process surface for "drive one request".
pub async fn handle_request(
    router: Arc<Router>,
    middleware_registry: Arc<MiddlewareRegistry>,
    req: hyper::Request<hyper::body::Incoming>,
) -> hyper::Response<ServerBody> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    // WebSocket upgrade branch. hyper-tungstenite checks the request
    // headers (Connection: Upgrade, Upgrade: websocket, Sec-WebSocket-*)
    // and returns true iff this is a well-formed WS upgrade. If a
    // ws_route matches the path, we hand off to handle_ws_upgrade;
    // the request never reaches the HTTP routing path. If no ws_route
    // matches, fall through to normal HTTP routing so the path can
    // 404 like any other unrouted GET.
    if hyper_tungstenite::is_upgrade_request(&req) && let Some(ws_match) = router.match_ws(&path) {
        return handle_ws_upgrade(req, ws_match).await;
    }

    let query = req.uri().query().unwrap_or("");

    // Built-in health check endpoint at /_suprnova/health
    // Uses framework prefix to avoid conflicts with user-defined routes
    if path == "/_suprnova/health" && method == hyper::Method::GET {
        return health_response(query).await;
    }

    // Inertia context comes off the live Request via header helpers
    // (`req.is_inertia()`, `req.inertia_version()`, etc.) — no global state.
    //
    // Per-request Inertia flash bag scoped via tokio::task_local. The bag
    // is drained by `InertiaResponse::resolve` at response build time.
    let flash_bag = crate::inertia::flash::new_bag();
    let ssr_disabled = crate::inertia::ssr::new_disable_ssr_flag();

    

    crate::inertia::flash::FLASH_BAG
        .scope(flash_bag, async move {
            crate::inertia::ssr::DISABLE_SSR
                .scope(ssr_disabled, async move {
                    handle_request_inner(router, middleware_registry, req, method, &path).await
                })
                .await
        })
        .await
}

async fn handle_request_inner(
    router: Arc<Router>,
    middleware_registry: Arc<MiddlewareRegistry>,
    req: hyper::Request<hyper::body::Incoming>,
    method: hyper::Method,
    path: &str,
) -> hyper::Response<ServerBody> {
    

    match router.match_route(&method, path) {
        Some((pattern, handler, params)) => {
            let request = Request::new(req).with_params(params);

            // Build middleware chain
            let mut chain = MiddlewareChain::new();

            // 0. RequestId is always outermost so spans + events emitted
            //    downstream carry the per-request id automatically.
            chain.push(into_boxed(RequestIdMiddleware));

            // 1. Add global middleware
            chain.extend(middleware_registry.global_middleware().iter().cloned());

            // 2. Add route-level middleware (already boxed).
            //    Lookup is keyed by `(method, pattern)` — the matched
            //    route pattern (e.g. `/api/posts/{id}`), NOT the raw
            //    request path. That keeps two invariants:
            //    (a) middleware registered for one HTTP method on a
            //        path never bleeds onto a sibling route on the
            //        same path under a different method; and
            //    (b) group-applied middleware on parameterised routes
            //        actually runs, instead of silently missing the
            //        lookup because `/api/posts/42 != /api/posts/{id}`.
            let route_middleware = router.get_route_middleware(&method, &pattern);
            chain.extend(route_middleware);

            // 3. Execute chain with handler
            let response = chain.execute(request, handler).await;

            // Unwrap the Result - both Ok and Err contain HttpResponse
            let http_response = response.unwrap_or_else(|e| e);
            // Mark the active tracing span as errored on 5xx so the
            // tracing-opentelemetry bridge translates it to OTel
            // `Status::Error`. The field is a no-op when no span is
            // active (e.g. before the Phase 2 request span lands).
            #[cfg(feature = "otel")]
            if http_response.status_code() >= 500 {
                tracing::Span::current().record("error", true);
            }
            http_response.into_hyper()
        }
        None => {
            // Check for fallback handler
            if let Some((fallback_handler, fallback_middleware)) = router.get_fallback() {
                let request = Request::new(req).with_params(std::collections::HashMap::new());

                // Build middleware chain for fallback
                let mut chain = MiddlewareChain::new();

                // 0. RequestId is always outermost (same as the matched-route path).
                chain.push(into_boxed(RequestIdMiddleware));

                // 1. Add global middleware
                chain.extend(middleware_registry.global_middleware().iter().cloned());

                // 2. Add fallback-specific middleware
                chain.extend(fallback_middleware);

                // 3. Execute chain with fallback handler
                let response = chain.execute(request, fallback_handler).await;

                // Unwrap the Result - both Ok and Err contain HttpResponse
                let http_response = response.unwrap_or_else(|e| e);
                // Mark the active tracing span as errored on 5xx so the
                // tracing-opentelemetry bridge translates it to OTel
                // `Status::Error`. The field is a no-op when no span is
                // active.
                #[cfg(feature = "otel")]
                if http_response.status_code() >= 500 {
                    tracing::Span::current().record("error", true);
                }
                http_response.into_hyper()
            } else {
                // No fallback defined, return default 404
                HttpResponse::text("404 Not Found").status(404).into_hyper()
            }
        }
    }
}

async fn handle_ws_upgrade(
    mut req: hyper::Request<hyper::body::Incoming>,
    ws_match: crate::routing::WsMatch,
) -> hyper::Response<ServerBody> {
    let handler = ws_match.handler();
    let params: HashMap<String, String> = ws_match.params().clone();

    let config = crate::ws::WsConfig::default();
    let tungstenite_config = config.to_tungstenite_config();

    let (response, websocket) =
        match hyper_tungstenite::upgrade(&mut req, Some(tungstenite_config)) {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(error = %e, "websocket upgrade rejected");
                return bad_request_text(&format!("websocket upgrade failed: {e}"));
            }
        };

    // Build the framework's Request from the upgrade request. The
    // body is empty for an upgrade request (RFC 6455); we still
    // construct via Request::new(req) so headers/cookies/session
    // are intact for the handler.
    let suprnova_req = Request::new(req).with_params(params);

    // Spawn the handler task. hyper switches the connection to
    // upgraded I/O once `response` flushes; then `websocket.await`
    // resolves with the upgraded WebSocketStream and we hand it
    // to the handler. Heartbeat task lands in T8 (it'll spawn here
    // and use socket.sender() + abort_handle for clean teardown).
    tokio::spawn(async move {
        match websocket.await {
            Ok(ws_stream) => {
                let socket = crate::ws::WsSocket::from_stream(ws_stream);
                if let Err(e) = handler.handle(socket, suprnova_req).await {
                    tracing::error!(error = %e, "websocket handler returned error");
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "hyper upgrade failed");
            }
        }
    });

    convert_response_body(response)
}

fn bad_request_text(msg: &str) -> hyper::Response<ServerBody> {
    hyper::Response::builder()
        .status(hyper::StatusCode::BAD_REQUEST)
        .body(
            Full::new(Bytes::from(msg.to_string()))
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("build 400 response")
}

fn convert_response_body(
    response: hyper::Response<http_body_util::Full<Bytes>>,
) -> hyper::Response<ServerBody> {
    let (parts, body) = response.into_parts();
    let boxed = body.map_err(|never| match never {}).boxed();
    hyper::Response::from_parts(parts, boxed)
}

/// Built-in health check endpoint at /_suprnova/health
/// Returns {"status": "ok", "timestamp": "..."} by default
/// Add ?db=true to also check database connectivity (/_suprnova/health?db=true)
async fn health_response(query: &str) -> hyper::Response<ServerBody> {
    use chrono::Utc;
    use serde_json::json;

    let timestamp = Utc::now().to_rfc3339();
    let check_db = query.contains("db=true");

    let mut response = json!({
        "status": "ok",
        "timestamp": timestamp
    });

    if check_db {
        // Try to check database connection
        match check_database_health().await {
            Ok(_) => {
                response["database"] = json!("connected");
            }
            Err(e) => {
                response["database"] = json!("error");
                response["database_error"] = json!(e);
            }
        }
    }

    let body = serde_json::to_string(&response).unwrap_or_else(|_| r#"{"status":"ok"}"#.to_string());

    hyper::Response::builder()
        .status(200)
        .header("Content-Type", "application/json")
        .body(
            Full::new(Bytes::from(body))
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap()
}

/// Check database health by attempting a simple query
async fn check_database_health() -> Result<(), String> {
    use crate::database::DB;
    use sea_orm::ConnectionTrait;

    if !DB::is_connected() {
        return Err("Database not initialized".to_string());
    }

    let conn = DB::connection().map_err(|e| e.to_string())?;

    // Execute a simple query to verify connection is alive
    conn.inner()
        .execute_unprepared("SELECT 1")
        .await
        .map_err(|e| format!("Database query failed: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Codex review finding #16: invalid `Server::host()` strings used to
    //! panic at boot via `host.parse().unwrap()`. These tests pin the
    //! current behaviour — a typed `FrameworkError` with a message that
    //! names the offending value and the expected format — so a future
    //! regression to `.unwrap()` fails loudly.
    use super::*;
    use crate::routing::Router;

    #[test]
    fn invalid_host_returns_typed_error_not_panic() {
        let server = Server::new(Router::new()).host("not-a-valid-host");
        let result = server.get_addr();
        let err = result.expect_err("invalid host must surface as Err, not panic or Ok");
        let msg = err.to_string();
        assert!(
            msg.contains("invalid server host"),
            "error message must identify the failure mode; got: {msg}"
        );
        assert!(
            msg.contains("not-a-valid-host"),
            "error message must echo the bad host value; got: {msg}"
        );
        // 500-class — internal misconfiguration, not a client error.
        assert_eq!(err.status_code(), 500);
    }

    #[test]
    fn empty_host_returns_typed_error_not_panic() {
        let server = Server::new(Router::new()).host("");
        let result = server.get_addr();
        let err = result.expect_err("empty host must surface as Err");
        let msg = err.to_string();
        assert!(
            msg.contains("invalid server host"),
            "error message must identify the failure mode; got: {msg}"
        );
    }

    #[test]
    fn valid_ipv4_host_parses_correctly() {
        let server = Server::new(Router::new()).host("127.0.0.1").port(8000);
        let addr = server.get_addr().expect("valid IPv4 should parse");
        assert_eq!(addr.to_string(), "127.0.0.1:8000");
    }

    #[test]
    fn valid_ipv6_host_parses_correctly() {
        let server = Server::new(Router::new()).host("::1").port(8000);
        let addr = server.get_addr().expect("valid IPv6 should parse");
        // SocketAddr renders IPv6 with bracket notation.
        assert_eq!(addr.to_string(), "[::1]:8000");
    }
}
