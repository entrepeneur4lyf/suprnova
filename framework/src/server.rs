use crate::cache::Cache;
use crate::config::{Config, ServerConfig};
use crate::container::App;
use crate::http::{HttpResponse, Request};
use crate::logging::{init_subscriber, LogConfig, RequestIdMiddleware};
use crate::middleware::{into_boxed, Middleware, MiddlewareChain, MiddlewareRegistry};
use crate::routing::Router;
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

    pub fn from_config(router: impl Into<Router>) -> Self {
        // Initialize the App container
        App::init();

        // Boot all auto-registered services from #[service(ConcreteType)]
        App::boot_services();

        let config = Config::get::<ServerConfig>().unwrap_or_else(ServerConfig::from_env);
        Self {
            router: Arc::new(router.into()),
            // Pull global middleware registered via global_middleware! in bootstrap.rs
            middleware: MiddlewareRegistry::from_global(),
            host: config.host,
            port: config.port,
        }
    }

    /// Add global middleware (runs on every request)
    ///
    /// For route-specific middleware, use `.middleware(M)` on the route itself.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Server::from_config(router)
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
        // Initialize the global tracing subscriber from env (idempotent).
        init_subscriber(LogConfig::from_env());

        // Bootstrap cache (Redis with in-memory fallback)
        Cache::bootstrap().await;

        let addr: SocketAddr = self.get_addr();
        let listener = TcpListener::bind(addr).await?;

        tracing::info!(%addr, "suprnova server listening");

        let router = self.router;
        let middleware = Arc::new(self.middleware);

        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);
            let router = router.clone();
            let middleware = middleware.clone();

            tokio::spawn(async move {
                let service = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let router = router.clone();
                    let middleware = middleware.clone();
                    async move { Ok::<_, Infallible>(handle_request(router, middleware, req).await) }
                });

                if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                    tracing::error!(?err, "error serving connection");
                }
            });
        }
    }
}

async fn handle_request(
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

    let response = crate::inertia::flash::FLASH_BAG
        .scope(flash_bag, async move {
            crate::inertia::ssr::DISABLE_SSR
                .scope(ssr_disabled, async move {
                    handle_request_inner(router, middleware_registry, req, method, &path).await
                })
                .await
        })
        .await;

    response
}

async fn handle_request_inner(
    router: Arc<Router>,
    middleware_registry: Arc<MiddlewareRegistry>,
    req: hyper::Request<hyper::body::Incoming>,
    method: hyper::Method,
    path: &str,
) -> hyper::Response<ServerBody> {
    let response = match router.match_route(&method, path) {
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
                http_response.into_hyper()
            } else {
                // No fallback defined, return default 404
                HttpResponse::text("404 Not Found").status(404).into_hyper()
            }
        }
    };

    response
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
