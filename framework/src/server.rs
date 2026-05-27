use crate::cache::Cache;
use crate::config::{Config, ServerConfig};
use crate::container::App;
use crate::http::{HttpResponse, Request};
use crate::lock;
use crate::logging::{LogConfig, RequestId, RequestIdMiddleware};
use crate::middleware::{Middleware, MiddlewareChain, MiddlewareRegistry, into_boxed};
use crate::routing::Router;
use crate::telemetry::{OtelConfig, init_telemetry};
use bytes::Bytes;
use futures::FutureExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, OnceLock};
use tokio::net::TcpListener;
use tokio::sync::Mutex as TokioMutex;
use tokio::task::JoinSet;
use tracing::Instrument;

/// Alias for the body type the server returns into hyper. All
/// `HttpResponse` variants (static + streaming) collapse to this so
/// the service signature stays uniform.
type ServerBody = BoxBody<Bytes, Infallible>;

/// Per-process registry of in-flight WebSocket handler tasks.
///
/// `handle_ws_upgrade` spawns each handler into this `JoinSet` so
/// `Server::run`'s shutdown sequence can drain them alongside the
/// HTTP connections JoinSet — without that, in-flight WS connections
/// get force-dropped on Ctrl-C / SIGTERM with no close frame to the
/// peer. Initialized lazily by `Server::run`; embedders that call
/// `handle_request` directly (T7 test fixture, custom hyper service
/// loops) get a bare `tokio::spawn` fallback so they don't have to
/// know about this registry.
static WS_TASKS: OnceLock<TokioMutex<JoinSet<()>>> = OnceLock::new();

pub struct Server {
    router: Arc<Router>,
    middleware: MiddlewareRegistry,
    host: String,
    port: u16,
}

impl Server {
    /// Build a [`Server`] with default host/port (`127.0.0.1:8000`).
    ///
    /// Pulls in middleware registered via `global_middleware!` (through
    /// [`MiddlewareRegistry::from_global`]), matching [`Server::from_config`]
    /// so global auth / session / logging applies no matter which
    /// constructor an embedder picks. (The two used to diverge: `new`
    /// started with an empty registry while `from_config` pulled globals —
    /// a silent way to ship a server with none of its global protection.)
    pub fn new(router: impl Into<Router>) -> Self {
        Self {
            router: Arc::new(router.into()),
            middleware: MiddlewareRegistry::from_global(),
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
    /// - `APP_KEY_PREVIOUS` is set and any comma-separated entry is
    ///   malformed. A half-rotated secret must fail at boot rather
    ///   than silently dropping the fallback key and leaving columns
    ///   undecryptable.
    ///
    /// Local, development, and testing environments generate a
    /// transient dev key when `APP_KEY` is unset, so `cargo run` stays
    /// zero-config. A loud `tracing::warn!` is emitted in that case so
    /// the operator notices sessions won't persist across restarts.
    ///
    /// `APP_KEY_PREVIOUS` (optional, comma-separated list of base64
    /// keys) configures decrypt fallback for key rotation. Encryption
    /// always uses the current `APP_KEY`; decryption tries current
    /// first, then each previous key in order. A `tracing::warn!` is
    /// emitted on every previous-key hit so the operator can schedule
    /// a re-encrypt pass and then remove the env var.
    pub fn from_config(router: impl Into<Router>) -> Result<Self, crate::FrameworkError> {
        // Initialize the App container
        App::init();

        // Boot all auto-registered services from #[service(ConcreteType)]
        // and #[injectable]. Propagates a structured error if a singleton's
        // dependency graph is unresolvable (missing #[injectable] or cycle).
        App::boot_services()?;

        // Install the process-wide encryption key ring.
        //
        // Production / staging / custom envs fail closed when APP_KEY
        // is missing — `resolve_boot_keyring` returns Err with an
        // actionable message that we propagate as the boot error.
        //
        // Local / development / testing fall through to a generated
        // transient current key. We log a loud warn so the operator
        // notices.
        //
        // `APP_KEY_PREVIOUS` is parsed alongside `APP_KEY` so a single
        // boot decision covers the whole ring; malformed previous-key
        // entries fail boot the same way a malformed `APP_KEY` does.
        //
        // Validate `APP_KEY` (+ `APP_KEY_PREVIOUS`) on EVERY boot, not just
        // when `Crypt` is uninitialized.
        //
        // Audit HIGH #334: the previous `if !Crypt::is_initialized()` gate
        // meant that any earlier key install (test hooks, embedders that
        // boot the server more than once, etc.) skipped the validation
        // entirely on subsequent boots — a missing/malformed APP_KEY in
        // production would slip through if any code path had pre-installed
        // a transient or test key. We now resolve the boot ring on every
        // call, so a production boot fails closed regardless of what may
        // have been installed earlier in the process.
        //
        // `init_with_keyring` is itself idempotent (no-op + `warn!` on
        // second call) so installing again after validation is safe; what
        // we get back is the freshly-validated key, even though the
        // installed ring still wins (sealed for the process lifetime).
        let environment = Config::get::<crate::config::AppConfig>()
            .map(|c| c.environment)
            .unwrap_or_else(crate::config::Environment::detect);
        let app_key = std::env::var("APP_KEY").ok();
        let app_key_previous = std::env::var("APP_KEY_PREVIOUS").ok();
        let boot_ring = crate::crypto::resolve_boot_keyring(
            &environment,
            app_key.as_deref(),
            app_key_previous.as_deref(),
        )?;

        // Only emit the dev-key / rotation-active operator hints on the
        // FIRST boot — repeated emissions on idempotent re-boot are noise.
        let first_boot = !crate::crypto::Crypt::is_initialized();
        if first_boot {
            if boot_ring.is_current_generated() {
                tracing::warn!(
                    environment = %environment,
                    "APP_KEY is not set — generated a transient development key. \
                     Sessions and cursors will reset on every restart. Set APP_KEY \
                     in your environment to persist them. This path is gated to \
                     local/development/testing; production fails closed."
                );
            }
            if !boot_ring.previous.is_empty() {
                tracing::info!(
                    previous_key_count = boot_ring.previous.len(),
                    "APP_KEY_PREVIOUS active — decrypt will fall back to {n} previous \
                     key(s). Run a re-encrypt pass (load + save every encrypted \
                     column) and then remove APP_KEY_PREVIOUS once complete.",
                    n = boot_ring.previous.len()
                );
            }
            let (current, previous) = boot_ring.into_keys();
            crate::crypto::Crypt::init_with_keyring(current, previous);
        }

        let config = Config::get::<ServerConfig>().unwrap_or_else(ServerConfig::from_env);

        // Domain 4 audit fix C1: wire the configured body cap into the
        // process-global atomic the request collector reads from. Before
        // this, `SERVER_MAX_BODY_SIZE=...` in the environment was parsed
        // into ServerConfig but silently ignored — the framework kept
        // using the compile-time default for every request body cap
        // decision. Per-FormRequest overrides still take precedence
        // because `FormRequest::max_body_bytes` is checked at extract
        // time, not at boot.
        crate::http::body::set_global_max_request_body_bytes(config.max_body_size);

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

        // Bootstrap cache — picks in-memory (default) or Redis based on
        // `CACHE_DRIVER`. Redis bootstrap fails closed on connect error;
        // no silent downgrade. See `Cache::bootstrap` for the contract.
        Cache::bootstrap().await?;

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

        // Initialize the WS handler-task registry so handle_ws_upgrade
        // can spawn into it instead of detaching via bare tokio::spawn.
        // `set` returns Err if already initialized (e.g. a previous
        // Server::run in the same process); that's fine — both servers
        // share the same drain registry and shutdown handles both.
        let _ = WS_TASKS.set(TokioMutex::new(JoinSet::new()));

        // Track in-flight connections so shutdown can drain them
        // before flushing OTel buffers. Each accepted connection is
        // spawned into this JoinSet rather than via bare tokio::spawn.
        let mut connections: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

        loop {
            tokio::select! {
                accept = listener.accept() => {
                    // Surviving transient accept errors keeps the server
                    // up under file-descriptor pressure, recoverable
                    // peer-side aborts (ECONNABORTED), and similar. The
                    // listener itself stays bound; only the per-connection
                    // accept failed.
                    //
                    // We log every transient error so persistent failures
                    // surface in operator dashboards, and apply a small
                    // backoff so a tight-loop failure mode (e.g. EMFILE
                    // until a connection drops) can't burn CPU. Truly
                    // fatal listener errors are extremely rare in
                    // practice; if they do happen, the per-iteration
                    // warn + 50ms sleep keeps the loop visible without
                    // dropping the server.
                    let (stream, _) = match accept {
                        Ok(pair) => pair,
                        Err(err) => {
                            tracing::warn!(
                                error = %err,
                                "accept error; continuing after 50ms backoff"
                            );
                            tokio::time::sleep(
                                std::time::Duration::from_millis(50),
                            )
                            .await;
                            continue;
                        }
                    };
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

        // Drain in-flight WebSocket handlers. These were spawned into
        // WS_TASKS by handle_ws_upgrade and are decoupled from the HTTP
        // connection JoinSet above (the connection task ends when the
        // 101 response flushes; the handler task runs independently).
        // Bound the drain window so a peer that never sends a close
        // frame can't block shutdown forever — after the deadline we
        // abort_all, which cancels the handler futures so the runtime
        // shutdown can proceed cleanly.
        if let Some(ws_tasks) = WS_TASKS.get() {
            let mut tasks = ws_tasks.lock().await;
            if !tasks.is_empty() {
                let in_flight = tasks.len();
                tracing::info!(
                    ws_in_flight = in_flight,
                    "draining in-flight WebSocket handlers (max 5s)"
                );
                let ws_drain_deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
                tokio::pin!(ws_drain_deadline);
                loop {
                    tokio::select! {
                        next = tasks.join_next() => {
                            if next.is_none() {
                                break; // JoinSet drained
                            }
                        }
                        _ = &mut ws_drain_deadline => {
                            tracing::warn!(
                                ws_in_flight = tasks.len(),
                                "WS drain deadline exceeded; aborting remaining handlers"
                            );
                            tasks.abort_all();
                            while tasks.join_next().await.is_some() {}
                            break;
                        }
                    }
                }
            }
        }

        // Signal supervisors to exit cleanly, then drain their tasks.
        // This runs AFTER WS_TASKS so in-flight WebSocket connections get
        // their close frames before the process tears down background work.
        crate::supervisor::SupervisorRegistry::shutdown(std::time::Duration::from_secs(5)).await;

        // Drain in-flight queued event listeners. These were spawned by
        // EventDispatcher for `queued()` events and run independently of the
        // request/worker that fired them; a deploy should let them finish
        // (bounded) rather than cut them off. Runs after supervisors so any
        // events they emit on the way down are caught, and before the telemetry
        // flush so listener spans land in the batch.
        let queued_in_flight =
            crate::events::EventFacade::drain_queued(std::time::Duration::from_secs(10)).await;
        if queued_in_flight > 0 {
            tracing::warn!(
                queued_listeners_in_flight = queued_in_flight,
                "queued event-listener drain deadline exceeded; aborted remaining tasks"
            );
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
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut sig) => {
            sig.recv().await;
        }
        Err(err) => {
            tracing::warn!(
                ?err,
                "failed to install SIGTERM handler; \
                Ctrl-C is still honored"
            );
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
    if hyper_tungstenite::is_upgrade_request(&req)
        && let Some(ws_match) = router.match_ws(&path)
    {
        return handle_ws_upgrade(req, ws_match, middleware_registry).await;
    }

    // Built-in health check endpoint at /_suprnova/health
    // Uses framework prefix to avoid conflicts with user-defined routes
    if path == "/_suprnova/health" && method == hyper::Method::GET {
        // The health endpoint short-circuits before the middleware chain,
        // so it resolves and echoes `X-Request-Id` itself to keep liveness
        // probes correlatable with logs — same contract as routed paths.
        let request = Request::new(req);
        let request_id = crate::logging::request_id::resolve_request_id(&request);
        let query = request.query().unwrap_or("").to_string();
        return health_response(&query, &request_id).await;
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
                    // Per-request auth state (resolved user cache + via-remember
                    // flag), guard-agnostic so token-only requests without a
                    // session can still use `set_user` / `once` / `has_user`.
                    crate::auth::request_state::scope(handle_request_inner(
                        router,
                        middleware_registry,
                        req,
                        method,
                        &path,
                    ))
                    .await
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

            // Resolve the request id ONCE for this request. The same id is
            // handed to the middleware (which scopes it) and to
            // `execute_chain_safely` (which echoes it on a synthesized 500
            // if the chain panics — the request scope is gone by then).
            let request_id = crate::logging::request_id::resolve_request_id(&request);

            // Build middleware chain
            let mut chain = MiddlewareChain::new();

            // 0. RequestId is always outermost so the `request` span it
            //    enters — and every event emitted downstream within it —
            //    carries the per-request id.
            chain.push(into_boxed(RequestIdMiddleware::with_id(request_id.clone())));

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

            // 3. Execute chain with handler, catching panics in middleware
            //    or handler so the client receives a proper 500 instead
            //    of a dropped connection.
            let http_response =
                execute_chain_safely(chain, request, handler, &method, path, request_id).await;

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
                let request_id = crate::logging::request_id::resolve_request_id(&request);

                // Build middleware chain for fallback
                let mut chain = MiddlewareChain::new();

                // 0. RequestId is always outermost (same as the matched-route path).
                chain.push(into_boxed(RequestIdMiddleware::with_id(request_id.clone())));

                // 1. Add global middleware
                chain.extend(middleware_registry.global_middleware().iter().cloned());

                // 2. Add fallback-specific middleware
                chain.extend(fallback_middleware);

                // 3. Execute chain with fallback handler, catching panics.
                let http_response = execute_chain_safely(
                    chain,
                    request,
                    fallback_handler,
                    &method,
                    path,
                    request_id,
                )
                .await;

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
                // No fallback handler registered. Still run the global
                // middleware chain (RequestId + global) terminating in a
                // fixed 404, so cross-cutting concerns act on unrouted
                // requests too: CORS preflight (OPTIONS never matches a
                // route, so it lands here) can short-circuit with its 204,
                // logging sees 404 traffic, and the response carries a
                // request id. This mirrors the fallback branch above — the
                // only difference is the terminal handler is a static 404
                // rather than a user-supplied fallback.
                let request = Request::new(req).with_params(std::collections::HashMap::new());
                let request_id = crate::logging::request_id::resolve_request_id(&request);

                let mut chain = MiddlewareChain::new();
                chain.push(into_boxed(RequestIdMiddleware::with_id(request_id.clone())));
                chain.extend(middleware_registry.global_middleware().iter().cloned());

                let not_found: Arc<crate::routing::BoxedHandler> =
                    Arc::new(Box::new(|_req: Request| {
                        Box::pin(async { Ok(HttpResponse::text("404 Not Found").status(404)) })
                            as std::pin::Pin<
                                Box<dyn std::future::Future<Output = crate::http::Response> + Send>,
                            >
                    }));

                let http_response =
                    execute_chain_safely(chain, request, not_found, &method, path, request_id)
                        .await;

                #[cfg(feature = "otel")]
                if http_response.status_code() >= 500 {
                    tracing::Span::current().record("error", true);
                }
                http_response.into_hyper()
            }
        }
    }
}

/// Run `chain.execute(request, handler)` with panic recovery.
///
/// A panic anywhere in the middleware stack or the route handler would
/// otherwise propagate up the per-connection task and tear down the
/// hyper service mid-response, leaving the client with a TCP reset and
/// no HTTP response. That's a hostile failure mode for an OSS framework
/// — a user-authored middleware calling `.unwrap()` on a `None` should
/// surface as a visible 500 the operator can debug, not a silent
/// connection drop. This helper catches the panic, logs it with the
/// request method + path for triage, and returns a 500 so the client
/// always gets a well-formed HTTP response.
///
/// `AssertUnwindSafe` is sound here because the captured state (chain,
/// request, handler) is internal framework data; users don't observe
/// partially-mutated state across the await boundary.
async fn execute_chain_safely(
    chain: MiddlewareChain,
    request: Request,
    handler: Arc<crate::routing::BoxedHandler>,
    method: &hyper::Method,
    path: &str,
    request_id: RequestId,
) -> HttpResponse {
    let exec = AssertUnwindSafe(chain.execute(request, handler));
    match exec.catch_unwind().await {
        Ok(result) => result.unwrap_or_else(|e| e),
        Err(panic) => {
            let msg = panic_payload_message(&panic);
            tracing::error!(
                panic = %msg,
                method = %method,
                path = %path,
                request_id = %request_id,
                "request middleware or handler panicked — translating to 500"
            );
            // Route the panic through the same `FrameworkError ->
            // HttpResponse` conversion that returned 5xx errors use:
            //   - the sanitised `{"message": "Internal Server Error"}`
            //     JSON body (no panic payload leaks downstream);
            //   - `ErrorOccurred` event dispatch, so observability
            //     listeners (Sentry, Pagerduty, custom log shippers) that
            //     fire on returned 5xx errors also fire on panics.
            // The panic message stays in the tracing::error! above, not in
            // the HTTP body — same 5xx-sanitisation contract.
            //
            // The panic unwound the original `REQUEST_ID` scope, so the
            // conversion (and the `ErrorOccurred` event it dispatches) would
            // otherwise read `current_request_id() == None`. Re-establish the
            // scope with the id resolved once before the chain ran so the
            // body, the generic 5xx log, and the event all stay correlatable,
            // then echo the same id back as `X-Request-Id`.
            crate::logging::REQUEST_ID
                .sync_scope(request_id.clone(), || {
                    HttpResponse::from(crate::error::FrameworkError::internal(format!(
                        "request handler panicked: {msg}"
                    )))
                })
                .header("X-Request-Id", request_id.as_str())
        }
    }
}

/// Extract a printable message from a panic payload returned by
/// `catch_unwind`. Panics in Rust are typed `Box<dyn Any + Send>`; the
/// common payload shapes are `&'static str` (literal panic messages)
/// and `String` (`format!`-built panic messages). Anything else
/// returns a generic placeholder so the logging path stays infallible.
fn panic_payload_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with non-string payload".to_string()
    }
}

async fn handle_ws_upgrade(
    mut req: hyper::Request<hyper::body::Incoming>,
    ws_match: crate::routing::WsMatch,
    middleware_registry: Arc<MiddlewareRegistry>,
) -> hyper::Response<ServerBody> {
    use crate::middleware::MiddlewareChain;
    use crate::routing::BoxedHandler;
    use std::sync::Mutex;

    let handler = ws_match.handler();
    let pattern = ws_match.pattern().to_string();
    let params: HashMap<String, String> = ws_match.params().clone();
    let middleware_list: Vec<crate::middleware::BoxedMiddleware> = ws_match.middleware().clone();

    let config = ws_match.config().cloned().unwrap_or_default();
    let heartbeat_interval = config.ping_interval;
    let tungstenite_config = config.to_tungstenite_config();

    // hyper_tungstenite::upgrade MUST come before Request::new because
    // it needs `&mut req` before we consume `req`.
    let (mut response, websocket) =
        match hyper_tungstenite::upgrade(&mut req, Some(tungstenite_config)) {
            Ok(pair) => pair,
            Err(e) => {
                tracing::warn!(error = %e, route = %pattern, "websocket upgrade rejected");
                return bad_request_text(&format!("websocket upgrade failed: {e}"));
            }
        };

    // Build the framework's Request from the upgrade request. The
    // body is empty for an upgrade request (RFC 6455); we still
    // construct via Request::new(req) so headers and cookies are
    // intact for the handler.
    let path = req.uri().path().to_string();
    let initial_request = Request::new(req).with_params(params);

    // Resolve the request id once for the whole upgrade. It is echoed on
    // the 101 handshake response, threaded into the connection span and
    // the post-upgrade session task, and — via `RequestIdMiddleware` —
    // attached to any rejection response the chain produces.
    let request_id = crate::logging::request_id::resolve_request_id(&initial_request);

    // A WebSocket upgrade is an HTTP GET, so the SAME middleware chain an
    // ordinary request gets applies here, in the SAME fixed order:
    // RequestId (outermost) -> global middleware -> per-route WS
    // middleware -> handler. Global auth / session / rate-limit / logging
    // protect `/ws/*` exactly as they protect any other route; they are
    // not silently skipped for upgrades.
    //
    // There is no empty-chain fast path anymore: RequestId and the globals
    // are always present, so the terminator-capture chain always runs. The
    // terminator records the final (possibly middleware-rewritten) Request
    // into a shared slot; a non-2xx response from any middleware (e.g. an
    // auth gate returning 401) aborts the upgrade and the unwoken websocket
    // future drops cleanly.
    //
    // Lock-poison handling: a panic inside a middleware would otherwise
    // poison the captured-request Mutex. We translate that into a 500 and
    // abort the upgrade rather than re-panicking inside the per-connection
    // task — one poisoned upgrade must not cascade into the accept loop or
    // other in-flight connections.
    let suprnova_req = {
        let captured: Arc<Mutex<Option<Request>>> = Arc::new(Mutex::new(None));
        let captured_for_terminator = captured.clone();

        let terminator: Arc<BoxedHandler> = Arc::new(Box::new(move |req: Request| {
            let captured = captured_for_terminator.clone();
            Box::pin(async move {
                match lock::lock(&captured) {
                    Ok(mut guard) => {
                        *guard = Some(req);
                        Ok(HttpResponse::text("").status(200))
                    }
                    Err(_) => Err(HttpResponse::text(
                        "internal error: websocket upgrade aborted (terminator lock poisoned)",
                    )
                    .status(500)),
                }
            })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = crate::http::Response> + Send>,
                >
        }));

        let mut chain = MiddlewareChain::new();
        chain.push(into_boxed(RequestIdMiddleware::with_id(request_id.clone())));
        chain.extend(middleware_registry.global_middleware().iter().cloned());
        chain.extend(middleware_list);

        // catch_unwind around the WS chain so a panicking middleware
        // can't tear down the upgrading connection task. On panic we
        // abort the upgrade with 500 — same policy as the HTTP request
        // path (see `execute_chain_safely`).
        let chain_response = match AssertUnwindSafe(chain.execute(initial_request, terminator))
            .catch_unwind()
            .await
        {
            Ok(resp) => resp,
            Err(panic) => {
                let msg = panic_payload_message(&panic);
                tracing::error!(
                    panic = %msg,
                    route = %pattern,
                    "websocket middleware panicked — aborting upgrade"
                );
                return HttpResponse::text(
                    "internal error: websocket upgrade aborted (middleware panicked)",
                )
                .status(500)
                .header("X-Request-Id", request_id.as_str())
                .into_hyper();
            }
        };

        // Response = Result<HttpResponse, HttpResponse>; collapse both
        // arms to a single HttpResponse the same way handle_request does.
        let http_response = chain_response.unwrap_or_else(|e| e);
        let status = http_response.status_code();
        if !(200..300).contains(&status) {
            // Middleware short-circuited (e.g. 401, 403). The response
            // already carries X-Request-Id: RequestIdMiddleware is the
            // outermost layer and tags both success and error variants.
            // Convert to ServerBody and return; the upgrade future drops
            // cleanly.
            tracing::debug!(
                status = status,
                route = %pattern,
                "websocket upgrade rejected by middleware"
            );
            return http_response.into_hyper();
        }

        match lock::lock(&captured) {
            Ok(mut guard) => match guard.take() {
                Some(req) => req,
                None => {
                    // Middleware chain returned 2xx without ever
                    // invoking `next(req)`. That's a programming bug
                    // in the middleware — abort the upgrade with a
                    // 500 so the issue is visible rather than the
                    // peer hanging on a stalled upgrade.
                    tracing::error!(
                        route = %pattern,
                        "websocket upgrade aborted: middleware chain \
                         returned 2xx without invoking `next(req)`"
                    );
                    return HttpResponse::text(
                        "internal error: websocket upgrade aborted (middleware did not call next)",
                    )
                    .status(500)
                    .header("X-Request-Id", request_id.as_str())
                    .into_hyper();
                }
            },
            Err(_) => {
                tracing::error!(
                    route = %pattern,
                    "websocket upgrade aborted: terminator lock poisoned"
                );
                return HttpResponse::text(
                    "internal error: websocket upgrade aborted (terminator lock poisoned)",
                )
                .status(500)
                .header("X-Request-Id", request_id.as_str())
                .into_hyper();
            }
        }
    };

    // Echo X-Request-Id on the 101 handshake response so the upgrade GET
    // stays correlatable with logs, the same contract as the HTTP path.
    // The id is `is_safe_request_id`-filtered (or a freshly minted UUID),
    // so building the header value cannot fail in practice — skip silently
    // on the impossible error rather than panicking on a header write.
    if let Ok(value) = hyper::header::HeaderValue::from_str(request_id.as_str()) {
        response.headers_mut().insert("x-request-id", value);
    }

    // Tracing span covers the entire WS connection lifecycle from
    // upgrade-resolved to handler-returned. It carries `request_id` so
    // every event the handler emits inherits it as span context (same
    // nested-`span` layout as the per-request span). Operators get a
    // single span per connection plus `connected` / `disconnected`
    // events bracketing the handler future.
    let span = tracing::info_span!(
        "ws.connection",
        request_id = %request_id,
        route = %pattern,
        path = %path
    );

    let handler_task = async move {
        match websocket.await {
            Ok(ws_stream) => {
                tracing::info!("websocket connected");

                let missed_pings = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let socket = crate::ws::WsSocket::from_stream_with_heartbeat(
                    ws_stream,
                    missed_pings.clone(),
                );
                // One bridge task feeds two senders: heartbeat clones
                // `outbound`, and we keep `outbound` itself for the
                // final close frame. When both senders drop, the
                // bridge task exits and the forwarder closes the sink.
                let outbound = socket.sender();
                let heartbeat_sender = outbound.clone();

                let heartbeat = tokio::spawn(crate::ws::heartbeat::run(
                    heartbeat_sender,
                    heartbeat_interval,
                    missed_pings,
                    config.max_missed_pings,
                ));
                let heartbeat_handle = heartbeat.abort_handle();

                let result = handler.handle(socket, suprnova_req).await;

                // Abort the heartbeat FIRST so its sender clone drops
                // and only `outbound` remains feeding the bridge.
                heartbeat_handle.abort();

                match result {
                    Ok(()) => {
                        // Send an explicit Close(1000) frame so the
                        // peer sees a normal-closure disconnect rather
                        // than the protocol-default 1005 ("No Status
                        // Received") that `sink.close()` alone produces.
                        // Best-effort: if the handler already called
                        // `socket.close()` the bridge has terminated
                        // and the send Errs — fine, we just move on.
                        let close = tokio_tungstenite::tungstenite::Message::Close(Some(
                            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                                code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
                                reason: tokio_tungstenite::tungstenite::Utf8Bytes::from_static(""),
                            },
                        ));
                        let _ = outbound.send(close).await;
                        tracing::info!("websocket disconnected (ok)");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "websocket handler returned error");
                    }
                }
                // Drop outbound so the bridge task exits and the
                // forwarder reads None → calls sink.close(), completing
                // the WebSocket close handshake.
                drop(outbound);
            }
            Err(e) => {
                tracing::error!(error = %e, "hyper upgrade failed");
            }
        }
    }
    .instrument(span);

    // The chain's REQUEST_ID scope unwound when `chain.execute` returned,
    // so `spawn_with_request_id` would capture nothing here. Re-establish
    // the id directly around the post-upgrade session task so the handler's
    // logs (and any work it spawns) carry the request id.
    let handler_task = crate::logging::REQUEST_ID.scope(request_id, handler_task);

    // Track the spawned handler in WS_TASKS so Server::run can drain
    // it on shutdown. Fall back to a bare tokio::spawn when WS_TASKS
    // isn't initialized (T7 test fixtures and external embedders that
    // call handle_request without going through Server::run).
    match WS_TASKS.get() {
        Some(tasks) => {
            let mut tasks = tasks.lock().await;
            // Opportunistic reap so the JoinSet doesn't grow unbounded
            // under long-running operation; completed handles get
            // dropped here instead of accumulating until shutdown.
            while tasks.try_join_next().is_some() {}
            tasks.spawn(handler_task);
        }
        None => {
            tokio::spawn(handler_task);
        }
    }

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

/// Built-in health check endpoint at /_suprnova/health.
///
/// Returns `{"status": "ok", "timestamp": "..."}` with HTTP 200 by
/// default. Add `?db=true` to also check database connectivity; if any
/// sub-check fails, the response status flips to 503 Service Unavailable
/// and the top-level `status` field changes to `"degraded"` so
/// k8s-style `livenessProbe` / `readinessProbe` configurations against
/// this endpoint can trigger restart on outage. The body shape (with
/// `database` and `database_error` fields) stays the same so dashboards
/// can parse both healthy and degraded responses uniformly.
async fn health_response(query: &str, request_id: &RequestId) -> hyper::Response<ServerBody> {
    use chrono::Utc;
    use serde_json::json;

    let timestamp = Utc::now().to_rfc3339();
    let check_db = query.contains("db=true");

    let mut response = json!({
        "status": "ok",
        "timestamp": timestamp
    });
    let mut degraded = false;

    if check_db {
        // Try to check database connection
        match check_database_health().await {
            Ok(_) => {
                response["database"] = json!("connected");
            }
            Err(e) => {
                response["database"] = json!("error");
                response["database_error"] = json!(e);
                response["status"] = json!("degraded");
                degraded = true;
            }
        }
    }

    let status = if degraded { 503 } else { 200 };
    let body = serde_json::to_string(&response).unwrap_or_else(|_| {
        if degraded {
            r#"{"status":"degraded"}"#.to_string()
        } else {
            r#"{"status":"ok"}"#.to_string()
        }
    });

    hyper::Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("X-Request-Id", request_id.as_str())
        .body(
            Full::new(Bytes::from(body))
                .map_err(|never| match never {})
                .boxed(),
        )
        .expect("health response builder must succeed for a static status + header set")
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
