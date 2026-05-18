use crate::http::{Request, Response};
use crate::middleware::{into_boxed, BoxedMiddleware, Middleware};
use crate::ws::BoxedWebSocketHandler;
use hyper::Method;
use matchit::Router as MatchitRouter;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};

/// Global registry mapping route names to path patterns
static ROUTE_REGISTRY: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

/// Register a route name -> path mapping
pub fn register_route_name(name: &str, path: &str) {
    let registry = ROUTE_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()));
    if let Ok(mut map) = registry.write() {
        map.insert(name.to_string(), path.to_string());
    }
}

/// Generate a URL for a named route with parameters
///
/// # Arguments
/// * `name` - The route name (e.g., "users.show")
/// * `params` - Slice of (key, value) tuples for path parameters
///
/// # Returns
/// * `Some(String)` - The generated URL with parameters substituted
/// * `None` - If the route name is not found
///
/// # Example
/// ```no_run
/// use suprnova::route;
///
/// let url = route("users.show", &[("id", "123")]);
/// assert_eq!(url, Some("/users/123".to_string()));
/// ```
pub fn route(name: &str, params: &[(&str, &str)]) -> Option<String> {
    let registry = ROUTE_REGISTRY.get()?.read().ok()?;
    let path_pattern = registry.get(name)?;

    let mut url = path_pattern.clone();
    for (key, value) in params {
        url = url.replace(&format!("{{{}}}", key), value);
    }
    Some(url)
}

/// Generate URL with HashMap parameters (used internally by Redirect)
pub fn route_with_params(name: &str, params: &HashMap<String, String>) -> Option<String> {
    let registry = ROUTE_REGISTRY.get()?.read().ok()?;
    let path_pattern = registry.get(name)?;

    let mut url = path_pattern.clone();
    for (key, value) in params {
        url = url.replace(&format!("{{{}}}", key), value);
    }
    Some(url)
}

/// Type alias for route handlers
pub type BoxedHandler =
    Box<dyn Fn(Request) -> Pin<Box<dyn Future<Output = Response> + Send>> + Send + Sync>;

/// HTTP Router with Laravel-like route registration
///
/// Each matchit per-method router stores `(pattern, handler)` so a
/// successful match returns both the resolved pattern (e.g.
/// `/api/posts/{id}`) and the handler. The pattern is what the
/// middleware registry is keyed by, so dispatch must look up
/// middleware under the matched pattern — not the raw request path
/// (e.g. `/api/posts/42`) — for group-applied middleware on
/// parameterised routes to run.
pub struct Router {
    get_routes: MatchitRouter<(String, Arc<BoxedHandler>)>,
    post_routes: MatchitRouter<(String, Arc<BoxedHandler>)>,
    put_routes: MatchitRouter<(String, Arc<BoxedHandler>)>,
    delete_routes: MatchitRouter<(String, Arc<BoxedHandler>)>,
    /// WebSocket route registry. Separate from the HTTP route
    /// registries because the handler type is different
    /// (`BoxedWebSocketHandler` vs `Arc<BoxedHandler>`) and the match
    /// returns a different shape (`WsMatch` instead of `(pattern,
    /// handler, params)`).
    ///
    /// Each entry stores `(pattern, handler, per_route_middleware)` so
    /// the per-route middleware chain can be recovered by `match_ws`
    /// and run in `handle_ws_upgrade` before dispatching the handler.
    ws_routes: MatchitRouter<(String, BoxedWebSocketHandler, Vec<BoxedMiddleware>)>,
    /// Middleware assignments: (method, path) -> boxed middleware instances.
    ///
    /// Keying by `(Method, String)` rather than path alone prevents
    /// middleware registered for one method on a path (e.g.
    /// `POST /api/posts` under an auth group) from silently bleeding
    /// onto a different method registered separately for the same
    /// path (e.g. a public `GET /api/posts`). This is a
    /// security-shaped invariant — the codex review tracked it as
    /// "route_middleware keyed by path leaks across methods".
    route_middleware: HashMap<(Method, String), Vec<BoxedMiddleware>>,
    /// Fallback handler for when no routes match (overrides default 404)
    fallback_handler: Option<Arc<BoxedHandler>>,
    /// Middleware for the fallback route
    fallback_middleware: Vec<BoxedMiddleware>,
}

impl Router {
    pub fn new() -> Self {
        Self {
            get_routes: MatchitRouter::new(),
            post_routes: MatchitRouter::new(),
            put_routes: MatchitRouter::new(),
            delete_routes: MatchitRouter::new(),
            ws_routes: MatchitRouter::new(),
            route_middleware: HashMap::new(),
            fallback_handler: None,
            fallback_middleware: Vec::new(),
        }
    }

    /// Get middleware registered for a specific `(method, pattern)` pair.
    ///
    /// The key is the HTTP method plus the route **pattern** that
    /// `add_middleware` was called with — e.g. `/api/posts/{id}`,
    /// not the resolved request path `/api/posts/42`. The dispatcher
    /// in `server.rs` calls `match_route` first to recover the matched
    /// pattern and then passes that pattern here, so parameterised
    /// routes inherit group middleware correctly.
    pub fn get_route_middleware(&self, method: &Method, pattern: &str) -> Vec<BoxedMiddleware> {
        self.route_middleware
            .get(&(method.clone(), pattern.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    /// Register middleware for a `(method, path)` pair (internal use).
    pub(crate) fn add_middleware(
        &mut self,
        method: Method,
        path: &str,
        middleware: BoxedMiddleware,
    ) {
        self.route_middleware
            .entry((method, path.to_string()))
            .or_default()
            .push(middleware);
    }

    /// Set the fallback handler for when no routes match
    pub(crate) fn set_fallback(&mut self, handler: Arc<BoxedHandler>) {
        self.fallback_handler = Some(handler);
    }

    /// Add middleware to the fallback route
    pub(crate) fn add_fallback_middleware(&mut self, middleware: BoxedMiddleware) {
        self.fallback_middleware.push(middleware);
    }

    /// Get the fallback handler and its middleware
    pub fn get_fallback(&self) -> Option<(Arc<BoxedHandler>, Vec<BoxedMiddleware>)> {
        self.fallback_handler
            .as_ref()
            .map(|h| (h.clone(), self.fallback_middleware.clone()))
    }

    /// Insert a GET route with a pre-boxed handler (internal use for groups)
    pub(crate) fn insert_get(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.get_routes
            .insert(path, (path.to_string(), handler))
            .ok();
    }

    /// Insert a POST route with a pre-boxed handler (internal use for groups)
    pub(crate) fn insert_post(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.post_routes
            .insert(path, (path.to_string(), handler))
            .ok();
    }

    /// Insert a PUT route with a pre-boxed handler (internal use for groups)
    pub(crate) fn insert_put(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.put_routes
            .insert(path, (path.to_string(), handler))
            .ok();
    }

    /// Insert a DELETE route with a pre-boxed handler (internal use for groups)
    pub(crate) fn insert_delete(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.delete_routes
            .insert(path, (path.to_string(), handler))
            .ok();
    }

    /// Register a GET route
    pub fn get<H, Fut>(mut self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.get_routes
            .insert(path, (path.to_string(), Arc::new(handler)))
            .ok();
        RouteBuilder {
            router: self,
            last_path: path.to_string(),
            last_method: Method::GET,
        }
    }

    /// Register a POST route
    pub fn post<H, Fut>(mut self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.post_routes
            .insert(path, (path.to_string(), Arc::new(handler)))
            .ok();
        RouteBuilder {
            router: self,
            last_path: path.to_string(),
            last_method: Method::POST,
        }
    }

    /// Register a PUT route
    pub fn put<H, Fut>(mut self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.put_routes
            .insert(path, (path.to_string(), Arc::new(handler)))
            .ok();
        RouteBuilder {
            router: self,
            last_path: path.to_string(),
            last_method: Method::PUT,
        }
    }

    /// Register a DELETE route
    pub fn delete<H, Fut>(mut self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.delete_routes
            .insert(path, (path.to_string(), Arc::new(handler)))
            .ok();
        RouteBuilder {
            router: self,
            last_path: path.to_string(),
            last_method: Method::DELETE,
        }
    }

    /// Register a WebSocket route. The handler runs after the
    /// framework completes the HTTP/1.1 Upgrade handshake; it
    /// receives a [`WsSocket`] plus the original [`Request`] so it
    /// can read cookies, session, headers, and captured route params.
    ///
    /// Unlike `get` / `post` / etc., this returns `Router` directly
    /// (not `RouteBuilder`) — WS routes support per-route middleware
    /// via [`Router::ws_with_middleware`] or the `ws!(...).middleware(M)`
    /// chain on `WsRouteDef`.
    ///
    /// [`WsSocket`]: crate::ws::WsSocket
    pub fn ws<H>(self, path: &str, handler: H) -> Router
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.ws_boxed_with_middleware(path, boxed, Vec::new())
    }

    /// Register a WebSocket route with a pre-populated middleware list.
    ///
    /// Middleware runs over the upgrade `Request` before the handler is
    /// dispatched. A non-2xx response from any middleware short-circuits
    /// the upgrade (the peer receives the rejection response and the
    /// WebSocket future drops cleanly). Middleware can substitute a
    /// modified `Request` via `next(modified_req)`.
    pub fn ws_with_middleware<H>(
        self,
        path: &str,
        handler: H,
        middleware: Vec<BoxedMiddleware>,
    ) -> Router
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.ws_boxed_with_middleware(path, boxed, middleware)
    }

    /// Register a pre-boxed WebSocket handler. Used internally by
    /// the `ws!` macro which type-erases the handler at the call
    /// site so the macro's `WsRouteDef` shape doesn't need a generic
    /// parameter.
    #[doc(hidden)]
    pub fn ws_boxed(self, path: &str, handler: BoxedWebSocketHandler) -> Router {
        self.ws_boxed_with_middleware(path, handler, Vec::new())
    }

    /// Register a pre-boxed WebSocket handler with a per-route middleware list.
    ///
    /// Used internally by `WsRouteDef::register` when middleware has been
    /// chained via `.middleware(M)`.
    #[doc(hidden)]
    pub fn ws_boxed_with_middleware(
        mut self,
        path: &str,
        handler: BoxedWebSocketHandler,
        middleware: Vec<BoxedMiddleware>,
    ) -> Router {
        self.ws_routes
            .insert(path, (path.to_string(), handler, middleware))
            .ok();
        self
    }

    /// Look up a WebSocket route by path. Returns the matched
    /// handler + captured params + per-route middleware if any route
    /// registered with [`Router::ws`] or [`Router::ws_with_middleware`]
    /// matches.
    pub fn match_ws(&self, path: &str) -> Option<WsMatch> {
        self.ws_routes.at(path).ok().map(|m| {
            let params: HashMap<String, String> = m
                .params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let (pattern, handler, middleware) = m.value;
            WsMatch {
                handler: handler.clone(),
                pattern: pattern.clone(),
                params,
                middleware: middleware.clone(),
            }
        })
    }

    /// Match a request and return the matched route's **pattern**, its
    /// handler, and the extracted path parameters.
    ///
    /// The pattern (e.g. `/api/posts/{id}`) is what the middleware
    /// registry is keyed by — pass it to
    /// [`Router::get_route_middleware`] so group-applied middleware on
    /// parameterised routes runs.
    pub fn match_route(
        &self,
        method: &hyper::Method,
        path: &str,
    ) -> Option<(String, Arc<BoxedHandler>, HashMap<String, String>)> {
        let router = match *method {
            hyper::Method::GET => &self.get_routes,
            hyper::Method::POST => &self.post_routes,
            hyper::Method::PUT => &self.put_routes,
            hyper::Method::DELETE => &self.delete_routes,
            _ => return None,
        };

        router.at(path).ok().map(|matched| {
            let params: HashMap<String, String> = matched
                .params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let (pattern, handler) = matched.value;
            (pattern.clone(), handler.clone(), params)
        })
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder returned after registering a route, enabling .name() chaining
pub struct RouteBuilder {
    pub(crate) router: Router,
    last_path: String,
    last_method: Method,
}

impl RouteBuilder {
    /// Name the most recently registered route
    pub fn name(self, name: &str) -> Router {
        register_route_name(name, &self.last_path);
        self.router
    }

    /// Apply middleware to the most recently registered route
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Router::new()
    ///     .get("/admin", admin_handler).middleware(AuthMiddleware)
    ///     .get("/api/users", users_handler).middleware(CorsMiddleware)
    /// ```
    pub fn middleware<M: Middleware + 'static>(mut self, middleware: M) -> RouteBuilder {
        let method = self.last_method.clone();
        let path = self.last_path.clone();
        self.router
            .add_middleware(method, &path, into_boxed(middleware));
        self
    }

    /// Apply pre-boxed middleware to the most recently registered route
    /// (Used internally by route macros)
    pub fn middleware_boxed(mut self, middleware: BoxedMiddleware) -> RouteBuilder {
        let method = self.last_method.clone();
        let path = self.last_path.clone();
        self.router.add_middleware(method, &path, middleware);
        self
    }

    /// Register a GET route (for chaining without .name())
    pub fn get<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.get(path, handler)
    }

    /// Register a POST route (for chaining without .name())
    pub fn post<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.post(path, handler)
    }

    /// Register a PUT route (for chaining without .name())
    pub fn put<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.put(path, handler)
    }

    /// Register a DELETE route (for chaining without .name())
    pub fn delete<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.delete(path, handler)
    }
}

impl From<RouteBuilder> for Router {
    fn from(builder: RouteBuilder) -> Self {
        builder.router
    }
}

/// Result of a [`Router::match_ws`] lookup. Bundles the matched
/// handler with the matched pattern (e.g. `/ws/rooms/{id}`),
/// the captured path parameters (e.g. `{"id": "42"}`), and
/// the per-route middleware list (empty if registered via plain
/// [`Router::ws`]).
pub struct WsMatch {
    handler: BoxedWebSocketHandler,
    pattern: String,
    params: HashMap<String, String>,
    middleware: Vec<BoxedMiddleware>,
}

impl WsMatch {
    /// The matched handler. Clone the `Arc` to move it into the
    /// spawned handler task.
    pub fn handler(&self) -> BoxedWebSocketHandler {
        self.handler.clone()
    }

    /// The registered route pattern that matched (e.g.
    /// `/ws/rooms/{id}`). Useful for telemetry / observability.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Captured path params as a borrowed `HashMap<String, String>`
    /// for handler consumption. The map is materialized once at
    /// match time so the lifetime is bound to the `WsMatch` itself,
    /// not the underlying matchit::Params reference. Callers that
    /// need owned data can `.clone()` at the call site.
    pub fn params(&self) -> &HashMap<String, String> {
        &self.params
    }

    /// Per-route middleware to run over the upgrade `Request` before
    /// dispatching to the handler. Empty slice when the route was
    /// registered without any `.middleware(M)` chaining.
    pub fn middleware(&self) -> &Vec<BoxedMiddleware> {
        &self.middleware
    }
}
