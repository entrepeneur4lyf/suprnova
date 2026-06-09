//! Route definition macros and helpers for Laravel-like routing syntax
//!
//! This module provides a clean, declarative way to define routes:
//!
//! ```rust,ignore
//! use suprnova::{routes, get, post, put, delete, group};
//!
//! routes! {
//!     get!("/", controllers::home::index).name("home"),
//!     get!("/users", controllers::user::index).name("users.index"),
//!     post!("/users", controllers::user::store).name("users.store"),
//!     get!("/protected", controllers::home::index).middleware(AuthMiddleware),
//!
//!     // Route groups with prefix and middleware
//!     group!("/api", {
//!         get!("/users", controllers::api::user::index).name("api.users.index"),
//!         post!("/users", controllers::api::user::store).name("api.users.store"),
//!     }).middleware(AuthMiddleware),
//! }
//! ```

use crate::http::{Request, Response};

/// Const function to validate route paths start with '/'
///
/// This provides compile-time validation that all route paths begin with '/'.
/// If a path doesn't start with '/', compilation will fail with a clear error.
///
/// # Panics
///
/// Panics at compile time if the path is empty or doesn't start with '/'.
pub const fn validate_route_path(path: &'static str) -> &'static str {
    let bytes = path.as_bytes();
    if bytes.is_empty() || bytes[0] != b'/' {
        panic!("Route path must start with '/'")
    }
    path
}
use crate::middleware::{BoxedMiddleware, Middleware, into_boxed};
use crate::routing::router::{BoxedHandler, Router, register_route_name};
use hyper::Method;
use std::future::Future;
use std::sync::Arc;

/// Convert Express-style `:param` route parameters to matchit-style `{param}`
///
/// This allows developers to use either syntax:
/// - `/:id` (Express/Rails style)
/// - `/{id}` (matchit native style)
///
/// # Segment-start matching
///
/// A `:` is only treated as a parameter opener when it sits at the start
/// of a path segment — either at the start of the pattern or immediately
/// after a `/`. A colon embedded *inside* a segment is preserved verbatim
/// so literal colons in path text (e.g. `/files/note:draft`) survive
/// untouched. This mirrors the Express / Rails convention where parameters
/// occupy whole segments and prevents the converter from over-capturing
/// the moment a path contains a literal `:`.
///
/// # Examples
///
/// - `/users/:id` → `/users/{id}`
/// - `/posts/:post_id/comments/:id` → `/posts/{post_id}/comments/{id}`
/// - `/users/{id}` → `/users/{id}` (already correct syntax, unchanged)
/// - `/files/note:draft` → `/files/note:draft` (mid-segment colon kept literal)
pub(crate) fn convert_route_params(path: &str) -> String {
    let mut result = String::with_capacity(path.len() + 4); // Extra space for braces
    let bytes = path.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];
        // Treat `:` as a parameter opener only at the start of a path
        // segment — first byte of the pattern, or the byte immediately
        // after a `/`. Mid-segment colons are literal text.
        let at_segment_start = i == 0 || bytes[i - 1] == b'/';
        if b == b':' && at_segment_start {
            result.push('{');
            i += 1;
            // Walk by UTF-8 code points, not raw bytes — `bytes[i] as char`
            // truncates each continuation byte into a stray Latin-1
            // codepoint, mojibaking any multi-byte param name (`/:café`
            // → `/{cafÃ©}`). Find the end of the segment, then slice the
            // original `&str` so multi-byte sequences survive intact.
            let seg_start = i;
            while i < bytes.len() && bytes[i] != b'/' {
                i += 1;
            }
            result.push_str(&path[seg_start..i]);
            result.push('}');
        } else {
            // Single-byte ASCII or the leading byte of a multi-byte UTF-8
            // sequence — either way, copy through to the next iteration.
            // Using char_indices would be safer for non-ASCII, but route
            // patterns are conventionally ASCII; fall back to slicing by
            // char boundary so any non-ASCII byte is still preserved.
            let ch_start = i;
            i += 1;
            while i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
                i += 1;
            }
            result.push_str(&path[ch_start..i]);
        }
    }
    result
}

/// Join a group prefix and a child path into one canonical route pattern.
///
/// Plain concatenation is wrong at the `/` boundary: a root group
/// (`group!("/", { get!("/login", …) })`) would register `//login`, which
/// matchit treats as containing an empty segment — no real request path
/// ever matches it, so every route in the group silently 404s. A
/// trailing-slash prefix (`/api/` + `/users`) has the same problem.
///
/// Rules:
/// - trailing `/`s are trimmed from the prefix, leading `/`s from the
///   child, and the two are joined with exactly one `/`
/// - a child of `/` (or empty) resolves to the prefix itself, so
///   `group!("/api", { get!("/", …) })` registers `/api`, not `/api/`
/// - a root prefix (`/` or empty) contributes nothing: `/` + `/login`
///   is `/login`
/// - both root resolves to `/`
///
/// The result always starts with `/`, so a child given without a leading
/// slash still produces a valid matchit pattern. Param conversion
/// (`:id` → `{id}`) runs on the joined result, never here.
pub(crate) fn join_paths(prefix: &str, child: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    let child = child.trim_start_matches('/');
    match (prefix.is_empty(), child.is_empty()) {
        (true, true) => "/".to_string(),
        (false, true) => prefix.to_string(),
        (_, false) => format!("{prefix}/{child}"),
    }
}

/// HTTP method for route definitions.
///
/// Mirrors the verbs the `Router` accepts. PATCH / HEAD / OPTIONS were
/// added as part of the verb-gap fix; HEAD requests with no explicit
/// HEAD route fall back to the GET registry inside
/// [`Router::match_route`] per RFC 9110 §9.3.2.
#[derive(Clone, Copy)]
pub enum HttpMethod {
    /// `GET` — safe, idempotent reads.
    Get,
    /// `POST` — create / non-idempotent submissions.
    Post,
    /// `PUT` — idempotent full replacement.
    Put,
    /// `PATCH` — partial updates.
    Patch,
    /// `DELETE` — resource removal.
    Delete,
    /// `HEAD` — `GET` without a body. Falls back to the `GET` registry when no explicit `HEAD` route is registered.
    Head,
    /// `OPTIONS` — capabilities discovery and CORS preflight.
    Options,
}

impl HttpMethod {
    /// Canonical `hyper::Method` for use as a `route_middleware` key.
    ///
    /// The middleware map is keyed by `(hyper::Method, String)` so that
    /// middleware on one method never bleeds onto a sibling route on
    /// the same path but a different method.
    fn as_hyper(self) -> Method {
        match self {
            HttpMethod::Get => Method::GET,
            HttpMethod::Post => Method::POST,
            HttpMethod::Put => Method::PUT,
            HttpMethod::Patch => Method::PATCH,
            HttpMethod::Delete => Method::DELETE,
            HttpMethod::Head => Method::HEAD,
            HttpMethod::Options => Method::OPTIONS,
        }
    }
}

/// Builder for route definitions that supports `.name()` and `.middleware()` chaining
pub struct RouteDefBuilder<H> {
    method: HttpMethod,
    path: &'static str,
    handler: H,
    name: Option<&'static str>,
    middlewares: Vec<BoxedMiddleware>,
}

impl<H, Fut> RouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    /// Create a new route definition builder
    pub fn new(method: HttpMethod, path: &'static str, handler: H) -> Self {
        Self {
            method,
            path,
            handler,
            name: None,
            middlewares: Vec::new(),
        }
    }

    /// Name this route for URL generation
    pub fn name(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }

    /// Add middleware to this route
    pub fn middleware<M: Middleware + 'static>(mut self, middleware: M) -> Self {
        self.middlewares.push(into_boxed(middleware));
        self
    }

    /// Register this route definition with a router
    pub fn register(self, router: Router) -> Router {
        // Convert :param to {param} for matchit compatibility
        let converted_path = convert_route_params(self.path);

        // First, register the route based on method
        let builder = match self.method {
            HttpMethod::Get => router.get(&converted_path, self.handler),
            HttpMethod::Post => router.post(&converted_path, self.handler),
            HttpMethod::Put => router.put(&converted_path, self.handler),
            HttpMethod::Patch => router.patch(&converted_path, self.handler),
            HttpMethod::Delete => router.delete(&converted_path, self.handler),
            HttpMethod::Head => router.head(&converted_path, self.handler),
            HttpMethod::Options => router.options(&converted_path, self.handler),
        };

        // Apply any middleware
        let builder = self
            .middlewares
            .into_iter()
            .fold(builder, |b, m| b.middleware_boxed(m));

        // Apply name if present, otherwise convert to Router
        if let Some(name) = self.name {
            builder.name(name)
        } else {
            builder.into()
        }
    }
}

/// Create a GET route definition with compile-time path validation
///
/// # Example
/// ```rust,ignore
/// get!("/users", controllers::user::index).name("users.index")
/// ```
///
/// # Compile Error
///
/// Fails to compile if path doesn't start with '/'.
#[macro_export]
macro_rules! get {
    ($path:expr, $handler:expr) => {{
        const _: &str = $crate::validate_route_path($path);
        $crate::__get_impl($path, $handler)
    }};
}

/// Internal implementation for GET routes (used by the get! macro)
#[doc(hidden)]
pub fn __get_impl<H, Fut>(path: &'static str, handler: H) -> RouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    RouteDefBuilder::new(HttpMethod::Get, path, handler)
}

/// Create a POST route definition with compile-time path validation
///
/// # Example
/// ```rust,ignore
/// post!("/users", controllers::user::store).name("users.store")
/// ```
///
/// # Compile Error
///
/// Fails to compile if path doesn't start with '/'.
#[macro_export]
macro_rules! post {
    ($path:expr, $handler:expr) => {{
        const _: &str = $crate::validate_route_path($path);
        $crate::__post_impl($path, $handler)
    }};
}

/// Internal implementation for POST routes (used by the post! macro)
#[doc(hidden)]
pub fn __post_impl<H, Fut>(path: &'static str, handler: H) -> RouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    RouteDefBuilder::new(HttpMethod::Post, path, handler)
}

/// Create a PUT route definition with compile-time path validation
///
/// # Example
/// ```rust,ignore
/// put!("/users/{id}", controllers::user::update).name("users.update")
/// ```
///
/// # Compile Error
///
/// Fails to compile if path doesn't start with '/'.
#[macro_export]
macro_rules! put {
    ($path:expr, $handler:expr) => {{
        const _: &str = $crate::validate_route_path($path);
        $crate::__put_impl($path, $handler)
    }};
}

/// Internal implementation for PUT routes (used by the put! macro)
#[doc(hidden)]
pub fn __put_impl<H, Fut>(path: &'static str, handler: H) -> RouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    RouteDefBuilder::new(HttpMethod::Put, path, handler)
}

/// Create a DELETE route definition with compile-time path validation
///
/// # Example
/// ```rust,ignore
/// delete!("/users/{id}", controllers::user::destroy).name("users.destroy")
/// ```
///
/// # Compile Error
///
/// Fails to compile if path doesn't start with '/'.
#[macro_export]
macro_rules! delete {
    ($path:expr, $handler:expr) => {{
        const _: &str = $crate::validate_route_path($path);
        $crate::__delete_impl($path, $handler)
    }};
}

/// Internal implementation for DELETE routes (used by the delete! macro)
#[doc(hidden)]
pub fn __delete_impl<H, Fut>(path: &'static str, handler: H) -> RouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    RouteDefBuilder::new(HttpMethod::Delete, path, handler)
}

/// Create a PATCH route definition with compile-time path validation.
///
/// PATCH is the standard verb for partial-resource updates (RFC 5789).
/// The macro shape mirrors `get!`/`post!`/`put!`/`delete!` and supports
/// `.name()` and `.middleware()` chaining.
///
/// # Example
/// ```rust,ignore
/// patch!("/users/{id}", controllers::user::update).name("users.patch")
/// ```
///
/// # Compile Error
///
/// Fails to compile if path doesn't start with '/'.
#[macro_export]
macro_rules! patch {
    ($path:expr, $handler:expr) => {{
        const _: &str = $crate::validate_route_path($path);
        $crate::__patch_impl($path, $handler)
    }};
}

/// Internal implementation for PATCH routes (used by the patch! macro)
#[doc(hidden)]
pub fn __patch_impl<H, Fut>(path: &'static str, handler: H) -> RouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    RouteDefBuilder::new(HttpMethod::Patch, path, handler)
}

/// Create a HEAD route definition with compile-time path validation.
///
/// HEAD requests with no explicit handler fall back to the GET registry
/// in [`Router::match_route`] per RFC 9110 §9.3.2; an explicit
/// `head!()` registration wins. The response body is stripped for HEAD
/// requests at the server boundary regardless of which arm matched, so
/// the handler can't accidentally emit content over the wire.
///
/// # Example
/// ```rust,ignore
/// head!("/cached", controllers::cache::head_probe)
/// ```
///
/// # Compile Error
///
/// Fails to compile if path doesn't start with '/'.
#[macro_export]
macro_rules! head {
    ($path:expr, $handler:expr) => {{
        const _: &str = $crate::validate_route_path($path);
        $crate::__head_impl($path, $handler)
    }};
}

/// Internal implementation for HEAD routes (used by the head! macro)
#[doc(hidden)]
pub fn __head_impl<H, Fut>(path: &'static str, handler: H) -> RouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    RouteDefBuilder::new(HttpMethod::Head, path, handler)
}

/// Create an OPTIONS route definition with compile-time path validation.
///
/// CORS preflight (`OPTIONS` + `Access-Control-Request-Method`) is
/// answered by `CorsMiddleware` at the global-middleware layer, before
/// the router. Use `options!()` for non-preflight uses — advertising
/// allowed verbs (`Accept-Patch`), public API discovery, programmatic
/// resource description.
///
/// # Example
/// ```rust,ignore
/// options!("/api/posts", controllers::api::post::discover)
/// ```
///
/// # Compile Error
///
/// Fails to compile if path doesn't start with '/'.
#[macro_export]
macro_rules! options {
    ($path:expr, $handler:expr) => {{
        const _: &str = $crate::validate_route_path($path);
        $crate::__options_impl($path, $handler)
    }};
}

/// Internal implementation for OPTIONS routes (used by the options! macro)
#[doc(hidden)]
pub fn __options_impl<H, Fut>(path: &'static str, handler: H) -> RouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    RouteDefBuilder::new(HttpMethod::Options, path, handler)
}

/// Create a route that responds to every common HTTP method —
/// `any!()` is the Laravel `Route::any(...)` equivalent. The handler
/// is registered against GET, POST, PUT, PATCH, DELETE, HEAD, and
/// OPTIONS sharing one matchit slot per method (per-method O(1)
/// dispatch). `.name()` registers the name once; `.middleware()` fans
/// out across every method's `(method, path)` middleware key so a
/// shared auth / CSRF / rate-limit guard cannot accidentally miss a
/// verb.
///
/// `any!` works at the top level of `routes! {}` and inside `group! {}`.
/// Inside a group, the group prefix is concatenated normally and the
/// group's inherited middleware fans out across every verb too.
///
/// # Example
/// ```rust,ignore
/// any!("/webhooks/inbound", controllers::webhooks::inbound)
///     .name("webhooks.inbound")
///     .middleware(SignatureCheck)
/// ```
///
/// # Compile Error
///
/// Fails to compile if path doesn't start with '/'.
#[macro_export]
macro_rules! any {
    ($path:expr, $handler:expr) => {{
        const _: &str = $crate::validate_route_path($path);
        $crate::__any_impl($path, $handler)
    }};
}

/// Internal implementation for `any!()` routes. Returns an
/// [`AnyRouteDefBuilder`] that records path + handler + optional name
/// + optional middlewares; the fan-out across seven methods happens
/// at `.register(router)` time.
#[doc(hidden)]
pub fn __any_impl<H, Fut>(path: &'static str, handler: H) -> AnyRouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    AnyRouteDefBuilder::new(path, handler)
}

/// Macro-layer builder for `any!()` routes. Symmetric with
/// [`RouteDefBuilder`] but registers across all seven common HTTP
/// methods at `register()` time. The `.name()` and `.middleware()`
/// chain methods accumulate state for the eventual fan-out — name
/// fires once, middleware fans out across every verb's
/// `(method, path)` middleware slot.
pub struct AnyRouteDefBuilder<H> {
    path: &'static str,
    handler: H,
    name: Option<&'static str>,
    middlewares: Vec<BoxedMiddleware>,
}

impl<H, Fut> AnyRouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn new(path: &'static str, handler: H) -> Self {
        Self {
            path,
            handler,
            name: None,
            middlewares: Vec::new(),
        }
    }

    /// Name this route. Registered once across all seven verbs since
    /// the path is shared.
    pub fn name(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }

    /// Attach middleware that runs for every method the `any!` route
    /// was registered against.
    pub fn middleware<M: Middleware + 'static>(mut self, middleware: M) -> Self {
        self.middlewares.push(into_boxed(middleware));
        self
    }

    /// Register this `any` route against the router. Drives
    /// [`Router::any`] then fans the middleware list across every
    /// method, then applies the optional name once.
    pub fn register(self, router: Router) -> Router {
        let converted_path = convert_route_params(self.path);
        let multi = router.any(&converted_path, self.handler);
        let multi = self
            .middlewares
            .into_iter()
            .fold(multi, |b, m| b.middleware_boxed(m));
        if let Some(name) = self.name {
            multi.name(name)
        } else {
            multi.into()
        }
    }
}

// ============================================================================
// WebSocket Route Support
// ============================================================================

/// Create a WebSocket route definition with compile-time path validation.
///
/// # Example
/// ```rust,ignore
/// ws!("/ws/echo", EchoHandler)
/// ```
///
/// Note: WebSocket routes do NOT support `.name()` or `.middleware()`
/// chaining in v1 — those land in Phase 7B alongside broadcasting
/// (specifically for per-WS-route auth middleware).
///
/// # Compile Error
///
/// Fails to compile if path doesn't start with '/'.
#[macro_export]
macro_rules! ws {
    ($path:expr, $handler:expr) => {{
        const _: &str = $crate::validate_route_path($path);
        $crate::__ws_impl($path, $handler)
    }};
}

/// Internal implementation for WebSocket routes (used by the `ws!` macro).
///
/// Type-erases the handler at the call site so `WsRouteDef` doesn't need
/// a generic parameter — the comma-separated `routes! { ... }` list can
/// then mix HTTP `RouteDefBuilder<H>` items and `WsRouteDef` items
/// without inference fights at the macro boundary.
#[doc(hidden)]
pub fn __ws_impl<H>(path: &'static str, handler: H) -> WsRouteDef
where
    H: crate::ws::WebSocketHandler,
{
    WsRouteDef::new(path, handler)
}

/// One WebSocket route item, produced by the `ws!` macro. The
/// `routes!` macro calls `register(router)` on each item to fold
/// them into a single `Router`.
///
/// Per-route middleware can be chained via `.middleware(M)`, and a
/// per-route [`WsConfig`] can be set via `.config(WsConfig)`:
///
/// ```rust,ignore
/// ws!("/ws/chat", ChatHandler)
///     .config(WsConfig { ping_interval: Duration::from_secs(5), ..Default::default() })
///     .middleware(SessionMiddleware::new())
/// ```
///
/// Both chains compose in any order. The middleware chain runs over
/// the upgrade `Request` before the handler is dispatched; a non-2xx
/// response short-circuits the upgrade.
///
/// [`WsConfig`]: crate::ws::WsConfig
pub struct WsRouteDef {
    path: &'static str,
    handler: crate::ws::BoxedWebSocketHandler,
    middleware: Vec<BoxedMiddleware>,
    config: Option<crate::ws::WsConfig>,
}

impl WsRouteDef {
    /// Create a new `WsRouteDef` for a typed handler. Used by
    /// `__ws_impl` (and therefore the `ws!` macro) to type-erase
    /// the handler at the call site.
    pub fn new<H>(path: &'static str, handler: H) -> Self
    where
        H: crate::ws::WebSocketHandler,
    {
        let boxed: crate::ws::BoxedWebSocketHandler = std::sync::Arc::new(handler);
        Self {
            path,
            handler: boxed,
            middleware: Vec::new(),
            config: None,
        }
    }

    /// Attach a middleware to this WS route. Multiple calls chain in
    /// registration order; all middleware run over the upgrade
    /// `Request` before the handler is dispatched.
    ///
    /// A non-2xx response from any middleware (e.g. `AuthMiddleware`
    /// returning 401) short-circuits the upgrade.
    pub fn middleware<M: Middleware + 'static>(mut self, m: M) -> Self {
        self.middleware.push(into_boxed(m));
        self
    }

    /// Override the default [`WsConfig`] for this route. Use to set
    /// per-route `ping_interval`, `max_message_size`, `max_frame_size`,
    /// or `max_missed_pings`. Routes without `.config(...)` use the
    /// framework default ([`WsConfig::default()`]).
    ///
    /// Can be chained before or after `.middleware(M)`:
    ///
    /// ```rust,ignore
    /// ws!("/ws/chat", ChatHandler)
    ///     .config(WsConfig { ping_interval: Duration::from_secs(5), ..Default::default() })
    ///     .middleware(SessionMiddleware::new())
    /// ```
    ///
    /// [`WsConfig`]: crate::ws::WsConfig
    /// [`WsConfig::default()`]: crate::ws::WsConfig::default
    pub fn config(mut self, config: crate::ws::WsConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Register this WS route on the given `Router`. Called by the
    /// `routes!` macro's expansion; not intended for direct use.
    pub fn register(self, router: Router) -> Router {
        router.ws_boxed_with_middleware_and_config(
            self.path,
            self.handler,
            self.middleware,
            self.config,
        )
    }
}

// ============================================================================
// Fallback Route Support
// ============================================================================

/// Builder for fallback route definitions that supports `.middleware()` chaining
///
/// The fallback route is invoked when no other routes match, allowing custom
/// handling of 404 scenarios.
pub struct FallbackDefBuilder<H> {
    handler: H,
    middlewares: Vec<BoxedMiddleware>,
}

impl<H, Fut> FallbackDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    /// Create a new fallback definition builder
    pub fn new(handler: H) -> Self {
        Self {
            handler,
            middlewares: Vec::new(),
        }
    }

    /// Add middleware to this fallback route
    pub fn middleware<M: Middleware + 'static>(mut self, middleware: M) -> Self {
        self.middlewares.push(into_boxed(middleware));
        self
    }

    /// Register this fallback definition with a router
    pub fn register(self, mut router: Router) -> Router {
        let handler = self.handler;
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        router.set_fallback(Arc::new(boxed));

        // Apply middleware
        for mw in self.middlewares {
            router.add_fallback_middleware(mw);
        }

        router
    }
}

/// Create a fallback route definition
///
/// The fallback handler is called when no other routes match the request,
/// allowing you to override the default 404 behavior.
///
/// # Example
/// ```rust,ignore
/// routes! {
///     get!("/", controllers::home::index),
///     get!("/users", controllers::user::index),
///
///     // Custom 404 handler
///     fallback!(controllers::fallback::invoke),
/// }
/// ```
///
/// With middleware:
/// ```rust,ignore
/// routes! {
///     get!("/", controllers::home::index),
///     fallback!(controllers::fallback::invoke).middleware(LoggingMiddleware),
/// }
/// ```
#[macro_export]
macro_rules! fallback {
    ($handler:expr) => {{ $crate::__fallback_impl($handler) }};
}

/// Internal implementation for fallback routes (used by the fallback! macro)
#[doc(hidden)]
pub fn __fallback_impl<H, Fut>(handler: H) -> FallbackDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    FallbackDefBuilder::new(handler)
}

// ============================================================================
// Route Grouping Support
// ============================================================================

/// A route stored within a group (type-erased handler)
pub struct GroupRoute {
    method: HttpMethod,
    path: &'static str,
    handler: Arc<BoxedHandler>,
    name: Option<&'static str>,
    middlewares: Vec<BoxedMiddleware>,
}

/// A multi-method route (`any!`) stored within a group. Holds a
/// pre-boxed handler shared across every verb plus the name +
/// middleware list. Registered by `GroupDef::register_with_inherited`
/// by fanning the handler into every per-method matchit slot and
/// fanning the middleware list across every `(method, path)` key.
pub struct GroupAnyRoute {
    path: &'static str,
    handler: Arc<BoxedHandler>,
    name: Option<&'static str>,
    middlewares: Vec<BoxedMiddleware>,
}

/// An item that can be added to a route group - a single-method route,
/// a multi-method (`any!`) route, or a nested group.
pub enum GroupItem {
    /// A single-method route
    Route(GroupRoute),
    /// A multi-method route (every common HTTP verb shares this handler)
    AnyRoute(GroupAnyRoute),
    /// A nested group with its own prefix and middleware
    NestedGroup(Box<GroupDef>),
}

/// Trait for types that can be converted into a GroupItem
pub trait IntoGroupItem {
    /// Convert `self` into a [`GroupItem`] suitable for collecting inside a route group.
    fn into_group_item(self) -> GroupItem;
}

/// Group definition that collects routes and applies prefix/middleware
///
/// Supports nested groups for arbitrary route organization:
///
/// ```rust,ignore
/// routes! {
///     group!("/api", {
///         get!("/users", controllers::user::index).name("api.users"),
///         post!("/users", controllers::user::store),
///         // Nested groups are supported
///         group!("/admin", {
///             get!("/dashboard", controllers::admin::dashboard),
///         }),
///     }).middleware(AuthMiddleware),
/// }
/// ```
pub struct GroupDef {
    prefix: &'static str,
    items: Vec<GroupItem>,
    group_middlewares: Vec<BoxedMiddleware>,
}

impl GroupDef {
    /// Create a new route group with the given prefix (internal use)
    ///
    /// Use the `group!` macro instead for compile-time validation.
    #[doc(hidden)]
    pub fn __new_unchecked(prefix: &'static str) -> Self {
        Self {
            prefix,
            items: Vec::new(),
            group_middlewares: Vec::new(),
        }
    }

    /// Add an item (route or nested group) to this group
    ///
    /// This is the primary method for adding items to a group. It accepts
    /// anything that implements `IntoGroupItem`, including routes created
    /// with `get!`, `post!`, etc., and nested groups created with `group!`.
    // `add` is the natural builder-method name here and is used throughout macro
    // emission and tests; renaming would cause excessive churn.
    #[allow(clippy::should_implement_trait)]
    pub fn add<I: IntoGroupItem>(mut self, item: I) -> Self {
        self.items.push(item.into_group_item());
        self
    }

    /// Add a route to this group (backward compatibility)
    ///
    /// Prefer using `.add()` which accepts both routes and nested groups.
    pub fn route<H, Fut>(self, route: RouteDefBuilder<H>) -> Self
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.add(route)
    }

    /// Add middleware to all routes in this group
    ///
    /// Middleware is applied in the order it's added.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// group!("/api", {
    ///     get!("/users", handler),
    /// }).middleware(AuthMiddleware).middleware(RateLimitMiddleware)
    /// ```
    pub fn middleware<M: Middleware + 'static>(mut self, middleware: M) -> Self {
        self.group_middlewares.push(into_boxed(middleware));
        self
    }

    /// Register all routes in this group with the router
    ///
    /// This prepends the group prefix to each route path and applies
    /// group middleware to all routes. Nested groups are flattened recursively,
    /// with prefixes concatenated and middleware inherited from parent groups.
    ///
    /// # Path Combination
    ///
    /// Prefix and route path are joined with a single canonical `/`
    /// boundary (see `join_paths`): a route path of `/` resolves to the
    /// group prefix itself, and a root prefix (`/`) contributes nothing,
    /// so `group!("/", { get!("/login", …) })` registers `/login` — never
    /// `//login`.
    ///
    /// # Middleware Inheritance
    ///
    /// Parent group middleware is applied before child group middleware,
    /// which is applied before route-specific middleware.
    pub fn register(self, mut router: Router) -> Router {
        self.register_with_inherited(&mut router, "", &[]);
        router
    }

    /// Internal recursive registration with inherited prefix and middleware
    fn register_with_inherited(
        self,
        router: &mut Router,
        parent_prefix: &str,
        inherited_middleware: &[BoxedMiddleware],
    ) {
        // Build the full prefix for this group. join_paths keeps the
        // `/` boundary canonical so a root parent (`group!("/")`) or a
        // trailing-slash prefix can't smuggle `//` into child routes.
        // The recursion seeds parent_prefix with "" — join_paths treats
        // that the same as a root `/` prefix.
        let full_prefix = join_paths(parent_prefix, self.prefix);

        // Combine inherited middleware with this group's middleware
        // Parent middleware runs first (outer), then this group's middleware
        let combined_middleware: Vec<BoxedMiddleware> = inherited_middleware
            .iter()
            .cloned()
            .chain(self.group_middlewares.iter().cloned())
            .collect();

        for item in self.items {
            match item {
                GroupItem::Route(route) => {
                    // Build full path with prefix, then convert :param to
                    // {param} for matchit compatibility. Conversion runs
                    // AFTER joining so a group prefix containing
                    // `:param` (e.g. `group!("/api/:version", { ... })`)
                    // gets normalised the same way the route segment does
                    // — without that, `:version` would reach matchit as a
                    // literal segment instead of a parameter.
                    let raw_full = join_paths(&full_prefix, route.path);
                    let full_path = convert_route_params(&raw_full);
                    // We need to leak the string to get a 'static str for the router
                    let full_path: &'static str = Box::leak(full_path.into_boxed_str());

                    // Register the route with the router
                    match route.method {
                        HttpMethod::Get => {
                            router.insert_get(full_path, route.handler);
                        }
                        HttpMethod::Post => {
                            router.insert_post(full_path, route.handler);
                        }
                        HttpMethod::Put => {
                            router.insert_put(full_path, route.handler);
                        }
                        HttpMethod::Patch => {
                            router.insert_patch(full_path, route.handler);
                        }
                        HttpMethod::Delete => {
                            router.insert_delete(full_path, route.handler);
                        }
                        HttpMethod::Head => {
                            router.insert_head(full_path, route.handler);
                        }
                        HttpMethod::Options => {
                            router.insert_options(full_path, route.handler);
                        }
                    }

                    // Register route name if present
                    if let Some(name) = route.name {
                        register_route_name(name, full_path);
                    }

                    // Apply combined middleware (inherited + group), then route-specific.
                    // The middleware map is keyed by `(method, path)` — middleware
                    // belongs to *this* route's method, never to siblings on the
                    // same path under a different method.
                    let http_method = route.method.as_hyper();
                    for mw in &combined_middleware {
                        router.add_middleware(http_method.clone(), full_path, mw.clone());
                    }
                    for mw in route.middlewares {
                        router.add_middleware(http_method.clone(), full_path, mw);
                    }
                }
                GroupItem::AnyRoute(any_route) => {
                    // Prefix join + matchit normalisation, mirroring the
                    // single-method arm above so a group prefix
                    // containing `:param` gets converted the same way.
                    let raw_full = join_paths(&full_prefix, any_route.path);
                    let full_path = convert_route_params(&raw_full);
                    let full_path: &'static str = Box::leak(full_path.into_boxed_str());

                    // Fan the same Arc<BoxedHandler> across every common
                    // HTTP method's matchit registry. Order matches
                    // `ANY_METHODS` in router.rs so tests / logs see the
                    // same ordering.
                    router.insert_get(full_path, any_route.handler.clone());
                    router.insert_post(full_path, any_route.handler.clone());
                    router.insert_put(full_path, any_route.handler.clone());
                    router.insert_patch(full_path, any_route.handler.clone());
                    router.insert_delete(full_path, any_route.handler.clone());
                    router.insert_head(full_path, any_route.handler.clone());
                    router.insert_options(full_path, any_route.handler);

                    // Name is registered once — the path is shared
                    // across all seven verbs so reverse-lookup returns
                    // the same URL no matter which method the caller
                    // is looking up.
                    if let Some(name) = any_route.name {
                        register_route_name(name, full_path);
                    }

                    // Fan combined (inherited + group) middleware AND
                    // route-local middleware across every (method, path)
                    // key. Without this, auth / CSRF / rate-limit
                    // attached to an `any!` route would silently skip
                    // some verbs.
                    let all_methods = [
                        hyper::Method::GET,
                        hyper::Method::POST,
                        hyper::Method::PUT,
                        hyper::Method::PATCH,
                        hyper::Method::DELETE,
                        hyper::Method::HEAD,
                        hyper::Method::OPTIONS,
                    ];
                    for method in &all_methods {
                        for mw in &combined_middleware {
                            router.add_middleware(method.clone(), full_path, mw.clone());
                        }
                        for mw in &any_route.middlewares {
                            router.add_middleware(method.clone(), full_path, mw.clone());
                        }
                    }
                }
                GroupItem::NestedGroup(nested) => {
                    // Recursively register the nested group with accumulated prefix and middleware
                    nested.register_with_inherited(router, &full_prefix, &combined_middleware);
                }
            }
        }
    }
}

impl<H, Fut> RouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    /// Convert this route definition to a type-erased GroupRoute
    ///
    /// This is used internally when adding routes to a group.
    pub fn into_group_route(self) -> GroupRoute {
        let handler = self.handler;
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        GroupRoute {
            method: self.method,
            path: self.path,
            handler: Arc::new(boxed),
            name: self.name,
            middlewares: self.middlewares,
        }
    }
}

// ============================================================================
// IntoGroupItem implementations
// ============================================================================

impl<H, Fut> IntoGroupItem for RouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn into_group_item(self) -> GroupItem {
        GroupItem::Route(self.into_group_route())
    }
}

impl IntoGroupItem for GroupDef {
    fn into_group_item(self) -> GroupItem {
        GroupItem::NestedGroup(Box::new(self))
    }
}

impl<H, Fut> AnyRouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    /// Convert this `any!()` definition to a type-erased `GroupAnyRoute`
    /// for use inside `group!{}`. Boxes the handler once; the seven-method
    /// fan-out happens inside `GroupDef::register_with_inherited`.
    pub fn into_group_any_route(self) -> GroupAnyRoute {
        let handler = self.handler;
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        GroupAnyRoute {
            path: self.path,
            handler: Arc::new(boxed),
            name: self.name,
            middlewares: self.middlewares,
        }
    }
}

impl<H, Fut> IntoGroupItem for AnyRouteDefBuilder<H>
where
    H: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn into_group_item(self) -> GroupItem {
        GroupItem::AnyRoute(self.into_group_any_route())
    }
}

/// Define a route group with a shared prefix
///
/// Routes within a group will have the prefix prepended to their paths.
/// Middleware can be applied to the entire group using `.middleware()`.
/// Groups can be nested arbitrarily deep.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{routes, get, post, group};
///
/// routes! {
///     get!("/", controllers::home::index),
///
///     // All routes in this group start with /api
///     group!("/api", {
///         get!("/users", controllers::user::index),      // -> GET /api/users
///         post!("/users", controllers::user::store),     // -> POST /api/users
///
///         // Nested groups are supported
///         group!("/admin", {
///             get!("/dashboard", controllers::admin::dashboard), // -> GET /api/admin/dashboard
///         }),
///     }).middleware(AuthMiddleware),  // Applies to ALL routes including nested
/// }
/// ```
///
/// # Middleware Inheritance
///
/// Middleware applied to a parent group is automatically inherited by all nested groups.
/// The execution order is: parent middleware -> child middleware -> route middleware.
///
/// # Compile Error
///
/// Fails to compile if prefix doesn't start with '/'.
#[macro_export]
macro_rules! group {
    ($prefix:expr, { $( $item:expr ),* $(,)? }) => {{
        const _: &str = $crate::validate_route_path($prefix);
        let mut group = $crate::GroupDef::__new_unchecked($prefix);
        $(
            group = group.add($item);
        )*
        group
    }};
}

/// Define routes with a clean, Laravel-like syntax
///
/// This macro generates a `pub fn register() -> Router` function automatically.
/// Place it at the top level of your `routes.rs` file.
///
/// # Example
/// ```rust,ignore
/// use suprnova::{routes, get, post, put, delete};
/// use crate::controllers;
/// use crate::middleware::AuthMiddleware;
///
/// routes! {
///     get!("/", controllers::home::index).name("home"),
///     get!("/users", controllers::user::index).name("users.index"),
///     get!("/users/{id}", controllers::user::show).name("users.show"),
///     post!("/users", controllers::user::store).name("users.store"),
///     put!("/users/{id}", controllers::user::update).name("users.update"),
///     delete!("/users/{id}", controllers::user::destroy).name("users.destroy"),
///     get!("/protected", controllers::home::index).middleware(AuthMiddleware),
/// }
/// ```
#[macro_export]
macro_rules! routes {
    ( $( $route:expr ),* $(,)? ) => {
        pub fn register() -> $crate::Router {
            let mut router = $crate::Router::new();
            $(
                router = $route.register(router);
            )*
            router
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_route_params() {
        // Basic parameter conversion
        assert_eq!(convert_route_params("/users/:id"), "/users/{id}");

        // Multiple parameters
        assert_eq!(
            convert_route_params("/posts/:post_id/comments/:id"),
            "/posts/{post_id}/comments/{id}"
        );

        // Already uses matchit syntax - should be unchanged
        assert_eq!(convert_route_params("/users/{id}"), "/users/{id}");

        // No parameters - should be unchanged
        assert_eq!(convert_route_params("/users"), "/users");
        assert_eq!(convert_route_params("/"), "/");

        // Mixed syntax (edge case)
        assert_eq!(
            convert_route_params("/users/:user_id/posts/{post_id}"),
            "/users/{user_id}/posts/{post_id}"
        );

        // Parameter at the end
        assert_eq!(
            convert_route_params("/api/v1/:version"),
            "/api/v1/{version}"
        );

        // Mid-segment colons are literal: only segment-leading `:` opens
        // a parameter. A literal colon inside a segment must survive
        // the conversion unchanged (`/foo:bar` stays `/foo:bar`,
        // `/files/note:draft` stays `/files/note:draft`).
        assert_eq!(convert_route_params("/foo:bar"), "/foo:bar");
        assert_eq!(
            convert_route_params("/files/note:draft"),
            "/files/note:draft"
        );
        // Mixed: segment-leading `:` still opens a param even when a
        // later segment carries a literal colon.
        assert_eq!(
            convert_route_params("/api/:version/files/note:draft"),
            "/api/{version}/files/note:draft"
        );

        // Multi-byte UTF-8 param names survive the conversion intact.
        // Pre-fix, the inner loop pushed `bytes[i] as char` per byte,
        // turning continuation bytes into stray Latin-1 codepoints
        // (`/:café` → `/{cafÃ©}`). Pin the correct behaviour so the
        // regression doesn't sneak back via a future "optimisation"
        // that swaps the str slice for a byte-by-byte copy again.
        assert_eq!(convert_route_params("/:café"), "/{café}");
        assert_eq!(
            convert_route_params("/users/:naïve/posts/:slug"),
            "/users/{naïve}/posts/{slug}"
        );
    }

    // Helper for creating test handlers
    async fn test_handler(_req: Request) -> Response {
        crate::http::text("ok")
    }

    #[test]
    fn test_group_item_route() {
        // Test that RouteDefBuilder can be converted to GroupItem
        let route_builder = RouteDefBuilder::new(HttpMethod::Get, "/test", test_handler);
        let item = route_builder.into_group_item();
        matches!(item, GroupItem::Route(_));
    }

    #[test]
    fn test_group_item_nested_group() {
        // Test that GroupDef can be converted to GroupItem
        let group_def = GroupDef::__new_unchecked("/nested");
        let item = group_def.into_group_item();
        matches!(item, GroupItem::NestedGroup(_));
    }

    #[test]
    fn test_group_add_route() {
        // Test adding a route to a group
        let group = GroupDef::__new_unchecked("/api").add(RouteDefBuilder::new(
            HttpMethod::Get,
            "/users",
            test_handler,
        ));

        assert_eq!(group.items.len(), 1);
        matches!(&group.items[0], GroupItem::Route(_));
    }

    #[test]
    fn test_group_add_nested_group() {
        // Test adding a nested group to a group
        let nested = GroupDef::__new_unchecked("/users");
        let group = GroupDef::__new_unchecked("/api").add(nested);

        assert_eq!(group.items.len(), 1);
        matches!(&group.items[0], GroupItem::NestedGroup(_));
    }

    #[test]
    fn test_group_mixed_items() {
        // Test adding both routes and nested groups
        let nested = GroupDef::__new_unchecked("/admin");
        let group = GroupDef::__new_unchecked("/api")
            .add(RouteDefBuilder::new(
                HttpMethod::Get,
                "/users",
                test_handler,
            ))
            .add(nested)
            .add(RouteDefBuilder::new(
                HttpMethod::Post,
                "/users",
                test_handler,
            ));

        assert_eq!(group.items.len(), 3);
        matches!(&group.items[0], GroupItem::Route(_));
        matches!(&group.items[1], GroupItem::NestedGroup(_));
        matches!(&group.items[2], GroupItem::Route(_));
    }

    #[test]
    fn test_deep_nesting() {
        // Test deeply nested groups (3 levels)
        let level3 = GroupDef::__new_unchecked("/level3").add(RouteDefBuilder::new(
            HttpMethod::Get,
            "/",
            test_handler,
        ));

        let level2 = GroupDef::__new_unchecked("/level2").add(level3);

        let level1 = GroupDef::__new_unchecked("/level1").add(level2);

        assert_eq!(level1.items.len(), 1);
        if let GroupItem::NestedGroup(l2) = &level1.items[0] {
            assert_eq!(l2.items.len(), 1);
            if let GroupItem::NestedGroup(l3) = &l2.items[0] {
                assert_eq!(l3.items.len(), 1);
            } else {
                panic!("Expected nested group at level 2");
            }
        } else {
            panic!("Expected nested group at level 1");
        }
    }

    #[test]
    fn test_backward_compatibility_route_method() {
        // Test that the old .route() method still works
        let group = GroupDef::__new_unchecked("/api").route(RouteDefBuilder::new(
            HttpMethod::Get,
            "/users",
            test_handler,
        ));

        assert_eq!(group.items.len(), 1);
        matches!(&group.items[0], GroupItem::Route(_));
    }

    // ---- PATCH / HEAD / OPTIONS macro coverage ------------------------

    /// `__patch_impl`, `__head_impl`, and `__options_impl` mint
    /// `RouteDefBuilder`s with the matching `HttpMethod` variant. The
    /// macros are thin wrappers that add compile-time path validation;
    /// once the impl fn is correct the macro is correct by construction.
    #[test]
    fn new_verb_impls_carry_correct_http_method() {
        let p = super::__patch_impl("/x", test_handler);
        assert!(matches!(p.method, HttpMethod::Patch));
        let h = super::__head_impl("/x", test_handler);
        assert!(matches!(h.method, HttpMethod::Head));
        let o = super::__options_impl("/x", test_handler);
        assert!(matches!(o.method, HttpMethod::Options));
    }

    /// PATCH / HEAD / OPTIONS variants of `HttpMethod` map to the
    /// matching `hyper::Method` so the middleware map keys correctly
    /// (`(Method::PATCH, path)` etc.). Without this, `.middleware(...)`
    /// on a PATCH route via the macro path would silently fail to
    /// register because the lookup key wouldn't match.
    #[test]
    fn new_verb_as_hyper_maps_to_matching_method() {
        assert_eq!(HttpMethod::Patch.as_hyper(), Method::PATCH);
        assert_eq!(HttpMethod::Head.as_hyper(), Method::HEAD);
        assert_eq!(HttpMethod::Options.as_hyper(), Method::OPTIONS);
    }

    /// `RouteDefBuilder::register` routes the new variants to the
    /// matching `Router::patch` / `head` / `options` calls. Drive each
    /// through the public macro chain and verify the resulting router
    /// matches.
    #[test]
    fn macros_register_new_verbs_via_route_def_builder() {
        use hyper::Method;
        let router: Router =
            RouteDefBuilder::new(HttpMethod::Patch, "/p", test_handler).register(Router::new());
        assert!(router.match_route(&Method::PATCH, "/p").is_some());

        let router: Router =
            RouteDefBuilder::new(HttpMethod::Head, "/h", test_handler).register(Router::new());
        assert!(router.match_route(&Method::HEAD, "/h").is_some());

        let router: Router =
            RouteDefBuilder::new(HttpMethod::Options, "/o", test_handler).register(Router::new());
        assert!(router.match_route(&Method::OPTIONS, "/o").is_some());
    }

    /// `GroupDef::register` flattens the new verbs into the right
    /// per-method registry, inheriting prefix the same way GET/POST do.
    /// Pins the new arms in `register_with_inherited`.
    #[test]
    fn group_def_registers_new_verbs_with_prefix() {
        use hyper::Method;
        let group = GroupDef::__new_unchecked("/api")
            .add(RouteDefBuilder::new(
                HttpMethod::Patch,
                "/users/:id",
                test_handler,
            ))
            .add(RouteDefBuilder::new(
                HttpMethod::Head,
                "/probes",
                test_handler,
            ))
            .add(RouteDefBuilder::new(
                HttpMethod::Options,
                "/discover",
                test_handler,
            ));

        let router = group.register(Router::new());

        assert!(
            router
                .match_route(&Method::PATCH, "/api/users/42")
                .is_some()
        );
        assert!(router.match_route(&Method::HEAD, "/api/probes").is_some());
        assert!(
            router
                .match_route(&Method::OPTIONS, "/api/discover")
                .is_some()
        );
    }

    #[test]
    fn join_paths_canonical_boundaries() {
        assert_eq!(join_paths("/", "/login"), "/login");
        assert_eq!(join_paths("", "/login"), "/login");
        assert_eq!(join_paths("/api", "/users"), "/api/users");
        assert_eq!(join_paths("/api/", "/users"), "/api/users");
        assert_eq!(join_paths("/api", "users"), "/api/users");
        assert_eq!(join_paths("/api", "/"), "/api");
        assert_eq!(join_paths("/api", ""), "/api");
        assert_eq!(join_paths("/", "/"), "/");
        assert_eq!(join_paths("", ""), "/");
        // Params pass through untouched — conversion runs on the result.
        assert_eq!(
            join_paths("/api/:version", "/users/:id"),
            "/api/:version/users/:id"
        );
    }

    /// Root-prefix group — the scaffold's `group!("/", { ... })` shape.
    /// Raw concatenation registered `//login`, which matchit treats as
    /// an empty segment: every route in the group 404'd over real HTTP.
    #[test]
    fn root_prefix_group_routes_match() {
        let group = GroupDef::__new_unchecked("/").add(RouteDefBuilder::new(
            HttpMethod::Get,
            "/login",
            test_handler,
        ));
        let router = group.register(Router::new());
        assert!(router.match_route(&Method::GET, "/login").is_some());
        assert!(router.match_route(&Method::GET, "//login").is_none());
    }

    /// Nested group under a root-prefix parent stays canonical at both
    /// join levels (`/` + `/admin` + `/settings` → `/admin/settings`).
    #[test]
    fn root_prefix_nested_group_routes_match() {
        let nested = GroupDef::__new_unchecked("/admin").add(RouteDefBuilder::new(
            HttpMethod::Get,
            "/settings",
            test_handler,
        ));
        let group = GroupDef::__new_unchecked("/").add(nested);
        let router = group.register(Router::new());
        assert!(
            router
                .match_route(&Method::GET, "/admin/settings")
                .is_some()
        );
    }

    /// Trailing-slash prefixes don't produce `//` either.
    #[test]
    fn trailing_slash_prefix_group_routes_match() {
        let group = GroupDef::__new_unchecked("/api/").add(RouteDefBuilder::new(
            HttpMethod::Get,
            "/users",
            test_handler,
        ));
        let router = group.register(Router::new());
        assert!(router.match_route(&Method::GET, "/api/users").is_some());
    }
}
