use crate::FrameworkError;
use crate::http::{Request, Response};
use crate::middleware::{BoxedMiddleware, Middleware, into_boxed};
use crate::ws::BoxedWebSocketHandler;
use hyper::Method;
use matchit::Router as MatchitRouter;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};

/// Global registry mapping route names to path patterns
static ROUTE_REGISTRY: OnceLock<RwLock<HashMap<String, String>>> = OnceLock::new();

/// Characters that must be percent-encoded when substituting a value
/// into a route pattern segment. The set covers the gen-delims and
/// sub-delims from RFC 3986 (`/ ? # [ ] @ ! $ & ' ( ) * + , ; =`) plus
/// the unsafe characters (space, `"`, `<`, `>`, `\`, `^`, `` ` ``, `{`,
/// `|`, `}`, `%`) and ASCII control codes.
///
/// Unreserved characters (`A-Z a-z 0-9 - _ . ~`) pass through unchanged,
/// matching what a browser sends for a URL-safe path segment.
const PATH_SEGMENT_ENCODE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'<')
    .add(b'>')
    .add(b'\\')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}')
    .add(b'%')
    // gen-delims
    .add(b':')
    .add(b'/')
    .add(b'?')
    .add(b'#')
    .add(b'[')
    .add(b']')
    .add(b'@')
    // sub-delims
    .add(b'!')
    .add(b'$')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b';')
    .add(b'=');

/// Register a route name -> path mapping.
///
/// # Panics
///
/// Panics if `name` is already registered under a different `path`.
/// Two routes resolving the same name silently shadowing each other is
/// a security-shaped bug — redirects and named-URL helpers would route
/// to whichever happened to win the registration race. Route names
/// must be unique; collisions fail loudly at boot.
///
/// Re-registering the same `(name, path)` pair is a no-op (idempotent).
/// Poisoned write locks are recovered via `PoisonError::into_inner` —
/// a panic during one thread's registration must not silently make
/// every subsequent name lookup return `None`.
pub fn register_route_name(name: &str, path: &str) {
    let registry = ROUTE_REGISTRY.get_or_init(|| RwLock::new(HashMap::new()));
    let mut map = match registry.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(existing) = map.get(name)
        && existing != path
    {
        panic!(
            "Route name '{name}' is already registered to path '{existing}'; \
             refusing to re-register to '{path}'. Route names must be unique \
             across the application — rename one of the routes.",
        );
    }
    map.insert(name.to_string(), path.to_string());
}

fn lookup_route(name: &str) -> Option<String> {
    let lock = ROUTE_REGISTRY.get()?;
    let map = match lock.read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    map.get(name).cloned()
}

fn substitute<F>(pattern: &str, mut next_value: F) -> String
where
    F: FnMut(&str) -> Option<String>,
{
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open + 1..];
        let Some(close) = rest.find('}') else {
            out.push('{');
            out.push_str(rest);
            return out;
        };
        let key = &rest[..close];
        if let Some(encoded) = next_value(key) {
            out.push_str(&encoded);
        } else {
            out.push('{');
            out.push_str(key);
            out.push('}');
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Generate a URL for a named route with parameters.
///
/// Path-parameter values are percent-encoded per RFC 3986 path-segment
/// rules so user-supplied content (slugs, IDs from query strings) cannot
/// inject path delimiters, query strings, or fragments into the resulting
/// URL. Unreserved characters (`A-Z a-z 0-9 - _ . ~`) pass through.
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
///
/// // Path-traversal attempts are encoded, not pasted in raw:
/// let url = route("users.show", &[("id", "../../etc/passwd")]);
/// assert_eq!(url, Some("/users/..%2F..%2Fetc%2Fpasswd".to_string()));
/// ```
pub fn route(name: &str, params: &[(&str, &str)]) -> Option<String> {
    let path_pattern = lookup_route(name)?;
    Some(substitute(&path_pattern, |key| {
        params
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| utf8_percent_encode(v, PATH_SEGMENT_ENCODE).to_string())
    }))
}

/// Generate URL with HashMap parameters (used internally by Redirect).
///
/// Path-parameter values are percent-encoded; see [`route`] for the
/// encoding policy.
pub fn route_with_params(name: &str, params: &HashMap<String, String>) -> Option<String> {
    let path_pattern = lookup_route(name)?;
    Some(substitute(&path_pattern, |key| {
        params
            .get(key)
            .map(|v| utf8_percent_encode(v, PATH_SEGMENT_ENCODE).to_string())
    }))
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
    /// Each entry stores `(pattern, handler, per_route_middleware,
    /// optional_ws_config)` so the per-route middleware chain and
    /// optional `WsConfig` override can be recovered by `match_ws`
    /// and used in `handle_ws_upgrade`.
    ws_routes: MatchitRouter<(
        String,
        BoxedWebSocketHandler,
        Vec<BoxedMiddleware>,
        Option<crate::ws::WsConfig>,
    )>,
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

    /// Insert a GET route with a pre-boxed handler (internal use for groups).
    ///
    /// # Panics
    ///
    /// Panics on duplicate route registration or any other matchit insert
    /// error (malformed pattern, too-many-segments, ...). Route registration
    /// is boot-time; silent swallowing of the second registration was a
    /// security-shaped bug because the surviving handler depended on
    /// registration order rather than user intent.
    pub(crate) fn insert_get(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.try_insert_get(path, handler)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// Fallible sibling of [`Router::insert_get`]: returns
    /// `Err(FrameworkError)` (naming the method + path) instead of panicking
    /// on a duplicate or malformed route pattern. Backs the public
    /// `try_*` registration surface and [`GroupBuilder::try_finalize`].
    pub(crate) fn try_insert_get(
        &mut self,
        path: &str,
        handler: Arc<BoxedHandler>,
    ) -> Result<(), FrameworkError> {
        self.get_routes
            .insert(path, (path.to_string(), handler))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register GET route '{path}': {e}"))
            })
    }

    /// Insert a POST route with a pre-boxed handler (internal use for groups).
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any other matchit insert error.
    /// See [`Router::insert_get`] for rationale.
    pub(crate) fn insert_post(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.try_insert_post(path, handler)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// Fallible sibling of [`Router::insert_post`]. See
    /// [`Router::try_insert_get`].
    pub(crate) fn try_insert_post(
        &mut self,
        path: &str,
        handler: Arc<BoxedHandler>,
    ) -> Result<(), FrameworkError> {
        self.post_routes
            .insert(path, (path.to_string(), handler))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register POST route '{path}': {e}"))
            })
    }

    /// Insert a PUT route with a pre-boxed handler (internal use for groups).
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any other matchit insert error.
    /// See [`Router::insert_get`] for rationale.
    pub(crate) fn insert_put(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.try_insert_put(path, handler)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// Fallible sibling of [`Router::insert_put`]. See
    /// [`Router::try_insert_get`].
    pub(crate) fn try_insert_put(
        &mut self,
        path: &str,
        handler: Arc<BoxedHandler>,
    ) -> Result<(), FrameworkError> {
        self.put_routes
            .insert(path, (path.to_string(), handler))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register PUT route '{path}': {e}"))
            })
    }

    /// Insert a DELETE route with a pre-boxed handler (internal use for groups).
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any other matchit insert error.
    /// See [`Router::insert_get`] for rationale.
    pub(crate) fn insert_delete(&mut self, path: &str, handler: Arc<BoxedHandler>) {
        self.try_insert_delete(path, handler)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    /// Fallible sibling of [`Router::insert_delete`]. See
    /// [`Router::try_insert_get`].
    pub(crate) fn try_insert_delete(
        &mut self,
        path: &str,
        handler: Arc<BoxedHandler>,
    ) -> Result<(), FrameworkError> {
        self.delete_routes
            .insert(path, (path.to_string(), handler))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register DELETE route '{path}': {e}"))
            })
    }

    /// Register a GET route.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically — `Router::new().get("/users/:id", h)`
    /// and the `get!("/users/:id", h)` macro produce identical
    /// `matchit` registrations.
    ///
    /// # Panics
    ///
    /// Panics on duplicate route registration (two handlers on the same
    /// pattern) or any matchit insert error. See [`Router::insert_get`].
    /// Use [`Router::try_get`] to get an `Err(FrameworkError)` instead of a
    /// panic when registering routes from a fallible source.
    pub fn get<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_get(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::get`]: returns `Err(FrameworkError)`
    /// (naming the method + path) on a duplicate or malformed pattern
    /// instead of panicking. The chain is consumed either way — on `Err`
    /// the partially-built router is dropped. Prefer this over [`Router::get`]
    /// when route patterns come from dynamic config, plugins, or any source
    /// you don't control at compile time.
    pub fn try_get<H, Fut>(mut self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let converted = crate::routing::macros::convert_route_params(path);
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.get_routes
            .insert(&converted, (converted.clone(), Arc::new(handler)))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register GET route '{path}': {e}"))
            })?;
        Ok(RouteBuilder {
            router: self,
            last_path: converted,
            last_method: Method::GET,
        })
    }

    /// Register a POST route.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    /// Use [`Router::try_post`] for a fallible variant.
    pub fn post<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_post(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::post`]. See [`Router::try_get`].
    pub fn try_post<H, Fut>(
        mut self,
        path: &str,
        handler: H,
    ) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let converted = crate::routing::macros::convert_route_params(path);
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.post_routes
            .insert(&converted, (converted.clone(), Arc::new(handler)))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register POST route '{path}': {e}"))
            })?;
        Ok(RouteBuilder {
            router: self,
            last_path: converted,
            last_method: Method::POST,
        })
    }

    /// Register a PUT route.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    /// Use [`Router::try_put`] for a fallible variant.
    pub fn put<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_put(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::put`]. See [`Router::try_get`].
    pub fn try_put<H, Fut>(mut self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let converted = crate::routing::macros::convert_route_params(path);
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.put_routes
            .insert(&converted, (converted.clone(), Arc::new(handler)))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register PUT route '{path}': {e}"))
            })?;
        Ok(RouteBuilder {
            router: self,
            last_path: converted,
            last_method: Method::PUT,
        })
    }

    /// Register a DELETE route.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration or any matchit insert error.
    /// Use [`Router::try_delete`] for a fallible variant.
    pub fn delete<H, Fut>(self, path: &str, handler: H) -> RouteBuilder
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_delete(path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::delete`]. See [`Router::try_get`].
    pub fn try_delete<H, Fut>(
        mut self,
        path: &str,
        handler: H,
    ) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let converted = crate::routing::macros::convert_route_params(path);
        let handler: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.delete_routes
            .insert(&converted, (converted.clone(), Arc::new(handler)))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register DELETE route '{path}': {e}"))
            })?;
        Ok(RouteBuilder {
            router: self,
            last_path: converted,
            last_method: Method::DELETE,
        })
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
        self.ws_boxed_with_middleware_and_config(path, boxed, Vec::new(), None)
    }

    /// Fallible sibling of [`Router::ws`]: returns `Err(FrameworkError)` on a
    /// duplicate or malformed pattern instead of panicking. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    pub fn try_ws<H>(self, path: &str, handler: H) -> Result<Router, FrameworkError>
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.try_ws_boxed_with_middleware_and_config(path, boxed, Vec::new(), None)
    }

    /// Register a WebSocket route with a per-route [`WsConfig`] override.
    ///
    /// Use to set per-route ping_interval, max_message_size, max_frame_size,
    /// or max_missed_pings. Routes without an explicit config use the
    /// framework default.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::time::Duration;
    /// use suprnova::ws::WsConfig;
    ///
    /// Router::new().ws_with_config("/ws/chat", ChatHandler, WsConfig {
    ///     ping_interval: Duration::from_secs(5),
    ///     ..Default::default()
    /// })
    /// ```
    ///
    /// [`WsConfig`]: crate::ws::WsConfig
    pub fn ws_with_config<H>(self, path: &str, handler: H, config: crate::ws::WsConfig) -> Router
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.ws_boxed_with_middleware_and_config(path, boxed, Vec::new(), Some(config))
    }

    /// Fallible sibling of [`Router::ws_with_config`]. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    pub fn try_ws_with_config<H>(
        self,
        path: &str,
        handler: H,
        config: crate::ws::WsConfig,
    ) -> Result<Router, FrameworkError>
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.try_ws_boxed_with_middleware_and_config(path, boxed, Vec::new(), Some(config))
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
        self.ws_boxed_with_middleware_and_config(path, boxed, middleware, None)
    }

    /// Fallible sibling of [`Router::ws_with_middleware`]. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    pub fn try_ws_with_middleware<H>(
        self,
        path: &str,
        handler: H,
        middleware: Vec<BoxedMiddleware>,
    ) -> Result<Router, FrameworkError>
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.try_ws_boxed_with_middleware_and_config(path, boxed, middleware, None)
    }

    /// Register a WebSocket route with both a middleware list and a per-route
    /// [`WsConfig`] override.
    pub fn ws_with_middleware_and_config<H>(
        self,
        path: &str,
        handler: H,
        middleware: Vec<BoxedMiddleware>,
        config: crate::ws::WsConfig,
    ) -> Router
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.ws_boxed_with_middleware_and_config(path, boxed, middleware, Some(config))
    }

    /// Fallible sibling of [`Router::ws_with_middleware_and_config`]. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    pub fn try_ws_with_middleware_and_config<H>(
        self,
        path: &str,
        handler: H,
        middleware: Vec<BoxedMiddleware>,
        config: crate::ws::WsConfig,
    ) -> Result<Router, FrameworkError>
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: BoxedWebSocketHandler = std::sync::Arc::new(handler);
        self.try_ws_boxed_with_middleware_and_config(path, boxed, middleware, Some(config))
    }

    /// Register a pre-boxed WebSocket handler. Used internally by
    /// the `ws!` macro which type-erases the handler at the call
    /// site so the macro's `WsRouteDef` shape doesn't need a generic
    /// parameter.
    #[doc(hidden)]
    pub fn ws_boxed(self, path: &str, handler: BoxedWebSocketHandler) -> Router {
        self.ws_boxed_with_middleware_and_config(path, handler, Vec::new(), None)
    }

    /// Fallible sibling of [`Router::ws_boxed`]. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    #[doc(hidden)]
    pub fn try_ws_boxed(
        self,
        path: &str,
        handler: BoxedWebSocketHandler,
    ) -> Result<Router, FrameworkError> {
        self.try_ws_boxed_with_middleware_and_config(path, handler, Vec::new(), None)
    }

    /// Register a pre-boxed WebSocket handler with a per-route middleware list.
    ///
    /// Used internally by `WsRouteDef::register` when middleware has been
    /// chained via `.middleware(M)`.
    #[doc(hidden)]
    pub fn ws_boxed_with_middleware(
        self,
        path: &str,
        handler: BoxedWebSocketHandler,
        middleware: Vec<BoxedMiddleware>,
    ) -> Router {
        self.ws_boxed_with_middleware_and_config(path, handler, middleware, None)
    }

    /// Fallible sibling of [`Router::ws_boxed_with_middleware`]. See
    /// [`Router::try_ws_boxed_with_middleware_and_config`].
    #[doc(hidden)]
    pub fn try_ws_boxed_with_middleware(
        self,
        path: &str,
        handler: BoxedWebSocketHandler,
        middleware: Vec<BoxedMiddleware>,
    ) -> Result<Router, FrameworkError> {
        self.try_ws_boxed_with_middleware_and_config(path, handler, middleware, None)
    }

    /// Register a pre-boxed WebSocket handler with a per-route middleware list
    /// and an optional [`WsConfig`] override. This is the canonical registration
    /// method — all other `ws*` variants delegate to this one.
    ///
    /// Express-style `:param` segments are converted to matchit-style
    /// `{param}` automatically — `Router::new().ws("/ws/rooms/:id", h)`
    /// and the `ws!("/ws/rooms/:id", h)` macro produce identical
    /// `matchit` registrations.
    ///
    /// # Panics
    ///
    /// Panics on duplicate route registration or any matchit insert error.
    /// WebSocket routes share the same boot-time-fail-loud policy as HTTP
    /// routes (see [`Router::insert_get`]).
    ///
    /// [`WsConfig`]: crate::ws::WsConfig
    #[doc(hidden)]
    pub fn ws_boxed_with_middleware_and_config(
        self,
        path: &str,
        handler: BoxedWebSocketHandler,
        middleware: Vec<BoxedMiddleware>,
        config: Option<crate::ws::WsConfig>,
    ) -> Router {
        self.try_ws_boxed_with_middleware_and_config(path, handler, middleware, config)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`Router::ws_boxed_with_middleware_and_config`]:
    /// returns `Err(FrameworkError)` (naming the path) on a duplicate or
    /// malformed pattern instead of panicking. This is the canonical
    /// fallible WebSocket registration primitive — every `try_ws*` helper
    /// delegates to it, mirroring the infallible family.
    #[doc(hidden)]
    pub fn try_ws_boxed_with_middleware_and_config(
        mut self,
        path: &str,
        handler: BoxedWebSocketHandler,
        middleware: Vec<BoxedMiddleware>,
        config: Option<crate::ws::WsConfig>,
    ) -> Result<Router, FrameworkError> {
        let converted = crate::routing::macros::convert_route_params(path);
        self.ws_routes
            .insert(&converted, (converted.clone(), handler, middleware, config))
            .map_err(|e| {
                FrameworkError::internal(format!("Failed to register WS route '{path}': {e}"))
            })?;
        Ok(self)
    }

    /// Look up a WebSocket route by path. Returns the matched
    /// handler + captured params + per-route middleware + optional
    /// per-route [`WsConfig`] if any route registered with
    /// [`Router::ws`], [`Router::ws_with_config`], or
    /// [`Router::ws_with_middleware`] matches.
    ///
    /// [`WsConfig`]: crate::ws::WsConfig
    pub fn match_ws(&self, path: &str) -> Option<WsMatch> {
        self.ws_routes.at(path).ok().map(|m| {
            let params: HashMap<String, String> = m
                .params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let (pattern, handler, middleware, config) = m.value;
            WsMatch {
                handler: handler.clone(),
                pattern: pattern.clone(),
                params,
                middleware: middleware.clone(),
                config: config.clone(),
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

    /// Fallible sibling of [`RouteBuilder::get`]. See [`Router::try_get`].
    pub fn try_get<H, Fut>(self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_get(path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::post`]. See [`Router::try_post`].
    pub fn try_post<H, Fut>(self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_post(path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::put`]. See [`Router::try_put`].
    pub fn try_put<H, Fut>(self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_put(path, handler)
    }

    /// Fallible sibling of [`RouteBuilder::delete`]. See
    /// [`Router::try_delete`].
    pub fn try_delete<H, Fut>(self, path: &str, handler: H) -> Result<RouteBuilder, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.router.try_delete(path, handler)
    }
}

impl From<RouteBuilder> for Router {
    fn from(builder: RouteBuilder) -> Self {
        builder.router
    }
}

/// Result of a [`Router::match_ws`] lookup. Bundles the matched
/// handler with the matched pattern (e.g. `/ws/rooms/{id}`),
/// the captured path parameters (e.g. `{"id": "42"}`), the
/// per-route middleware list (empty if registered via plain
/// [`Router::ws`]), and an optional per-route [`WsConfig`] override.
///
/// [`WsConfig`]: crate::ws::WsConfig
pub struct WsMatch {
    handler: BoxedWebSocketHandler,
    pattern: String,
    params: HashMap<String, String>,
    middleware: Vec<BoxedMiddleware>,
    config: Option<crate::ws::WsConfig>,
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

    /// Per-route [`WsConfig`] override, if the route was registered
    /// with `.config(WsConfig)` or [`Router::ws_with_config`].
    /// Returns `None` when no override was set — the caller should
    /// fall back to [`WsConfig::default()`].
    ///
    /// [`WsConfig`]: crate::ws::WsConfig
    /// [`WsConfig::default()`]: crate::ws::WsConfig::default
    pub fn config(&self) -> Option<&crate::ws::WsConfig> {
        self.config.as_ref()
    }
}

#[cfg(test)]
mod tests {
    //! Domain 1 audit regressions (2026-05).
    //!
    //! Findings F1, F5, F6, F7, F8 from
    //! `docs/superpowers/audit-2026-05/DOMAIN-01-router-and-dispatch.md`.

    use super::*;
    use crate::http::text;

    async fn h(_req: Request) -> Response {
        text("ok")
    }

    /// F1: duplicate route registration panics instead of silently dropping
    /// the second handler.
    #[test]
    #[should_panic(expected = "Failed to register GET route '/users'")]
    fn duplicate_get_registration_panics() {
        let _ = Router::new().get("/users", h).get("/users", h);
    }

    /// F1: insert_post / put / delete share the same policy.
    #[test]
    #[should_panic(expected = "Failed to register DELETE route '/x'")]
    fn duplicate_delete_registration_panics() {
        let _ = Router::new().delete("/x", h).delete("/x", h);
    }

    /// F6: fluent `Router::get(":id", h)` registers under the matchit-shape
    /// pattern `/users/{id}` and captures `id` from the request path.
    /// Previously the fluent path passed `:id` through verbatim and matchit
    /// treated it as a literal segment, so `/users/42` failed to match.
    #[test]
    fn fluent_path_supports_colon_param_syntax() {
        let router: Router = Router::new().get("/users/:id", h).into();
        let m = router.match_route(&Method::GET, "/users/42");
        let (pattern, _handler, params) = m.expect(
            "fluent :id syntax must match /users/42 \
             — convert_route_params should run in the fluent path too",
        );
        assert_eq!(pattern, "/users/{id}");
        assert_eq!(params.get("id"), Some(&"42".to_string()));
    }

    /// F6 + F5 + F7 + name resolution: a fluent `:id` route registered with
    /// `.name(...)` must resolve via `route(name, &[...])` to the same URL
    /// the macro path would produce.
    #[test]
    fn fluent_named_colon_route_resolves_via_route_helper() {
        let _ = Router::new()
            .get("/posts/:slug", h)
            .name("posts.show.fluent");
        let url = route("posts.show.fluent", &[("slug", "hello-world")]);
        assert_eq!(url, Some("/posts/hello-world".to_string()));
    }

    /// F5: path-segment values containing reserved characters are
    /// percent-encoded. Without this fix, a user-controlled slug
    /// containing `/`, `?`, `#`, or `&` would corrupt the generated URL
    /// (open-redirect / path-injection class).
    #[test]
    fn route_percent_encodes_reserved_path_characters() {
        let _ = Router::new()
            .get("/users/{id}", h)
            .name("users.show.encoding");
        let url = route("users.show.encoding", &[("id", "../../etc/passwd")]);
        assert_eq!(
            url.as_deref(),
            Some("/users/..%2F..%2Fetc%2Fpasswd"),
            "slash characters must be percent-encoded out of path segments",
        );
    }

    /// F5: question-mark, hash, ampersand are not allowed to inject query
    /// string or fragment into the URL.
    #[test]
    fn route_percent_encodes_query_and_fragment_delimiters() {
        let _ = Router::new()
            .get("/posts/{slug}", h)
            .name("posts.show.frag");
        let url = route("posts.show.frag", &[("slug", "x?evil=1#hash&y=2")]);
        assert_eq!(
            url.as_deref(),
            Some("/posts/x%3Fevil%3D1%23hash%26y%3D2"),
            "?, #, =, & must be percent-encoded so they cannot inject \
             query string or fragment into the generated URL",
        );
    }

    /// F5: unreserved characters pass through unchanged so URLs stay
    /// readable when the slug is well-formed.
    #[test]
    fn route_preserves_unreserved_characters() {
        let _ = Router::new()
            .get("/posts/{slug}", h)
            .name("posts.show.safe");
        let url = route("posts.show.safe", &[("slug", "hello-world_42.html~tilde")]);
        assert_eq!(url.as_deref(), Some("/posts/hello-world_42.html~tilde"),);
    }

    /// F7: registering the same name to a different path panics. Two
    /// routes claiming the same name silently shadowing each other is a
    /// security-shaped bug.
    #[test]
    #[should_panic(expected = "Route name 'users.duplicate'")]
    fn duplicate_route_name_panics() {
        let _ = Router::new()
            .get("/a", h)
            .name("users.duplicate")
            .get("/b", h)
            .name("users.duplicate");
    }

    /// F7: registering the same `(name, path)` pair twice is idempotent.
    /// This matters for inventory-driven registration where the same
    /// route may be registered on every test that calls
    /// `routes::register()`.
    #[test]
    fn registering_same_name_same_path_is_idempotent() {
        register_route_name("idempotent.example", "/foo/{id}");
        register_route_name("idempotent.example", "/foo/{id}");
        let url = route("idempotent.example", &[("id", "1")]);
        assert_eq!(url, Some("/foo/1".to_string()));
    }

    /// F5: `route_with_params` (HashMap path) shares the same encoding.
    #[test]
    fn route_with_params_percent_encodes_values() {
        let _ = Router::new()
            .get("/redirect/{target}", h)
            .name("redirect.target");
        let params: HashMap<String, String> =
            [("target".to_string(), "https://evil.example/".to_string())]
                .into_iter()
                .collect();
        let url = route_with_params("redirect.target", &params);
        assert_eq!(
            url.as_deref(),
            Some("/redirect/https%3A%2F%2Fevil.example%2F"),
        );
    }

    /// F5: missing parameter values leave the `{name}` placeholder intact.
    /// Previously the substitution silently produced a partial URL with
    /// `{name}` still embedded; verify the behaviour stays the same
    /// (callers can detect the missing param visually) and that the
    /// re-encoded form contains the original placeholder unchanged.
    #[test]
    fn route_leaves_unfilled_placeholders_in_place() {
        let _ = Router::new().get("/{a}/{b}", h).name("two.params.test");
        let url = route("two.params.test", &[("a", "x")]);
        assert_eq!(url, Some("/x/{b}".to_string()));
    }
}
