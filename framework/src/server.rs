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

    fn get_addr(&self) -> SocketAddr {
        SocketAddr::new(self.host.parse().unwrap(), self.port)
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

        let addr: SocketAddr = self.get_addr();
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

                        if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
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
        Some((handler, params)) => {
            let request = Request::new(req).with_params(params);

            // Build middleware chain
            let mut chain = MiddlewareChain::new();

            // 0. RequestId is always outermost so spans + events emitted
            //    downstream carry the per-request id automatically.
            chain.push(into_boxed(RequestIdMiddleware));

            // 1. Add global middleware
            chain.extend(middleware_registry.global_middleware().iter().cloned());

            // 2. Add route-level middleware (already boxed)
            let route_middleware = router.get_route_middleware(path);
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
