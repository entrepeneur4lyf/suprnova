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
use crate::middleware::{into_boxed, BoxedMiddleware, Middleware};
use crate::routing::router::{register_route_name, BoxedHandler, Router};
use hyper::Method;
use std::future::Future;
use std::sync::Arc;

/// Convert Express-style `:param` route parameters to matchit-style `{param}`
///
/// This allows developers to use either syntax:
/// - `/:id` (Express/Rails style)
/// - `/{id}` (matchit native style)
///
/// # Examples
///
/// - `/users/:id` → `/users/{id}`
/// - `/posts/:post_id/comments/:id` → `/posts/{post_id}/comments/{id}`
/// - `/users/{id}` → `/users/{id}` (already correct syntax, unchanged)
fn convert_route_params(path: &str) -> String {
    let mut result = String::with_capacity(path.len() + 4); // Extra space for braces
    let mut chars = path.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == ':' {
            // Start of parameter - collect until '/' or end
            result.push('{');
            while let Some(&next) = chars.peek() {
                if next == '/' {
                    break;
                }
                result.push(chars.next().unwrap());
            }
            result.push('}');
        } else {
            result.push(ch);
        }
    }
    result
}

/// HTTP method for route definitions
#[derive(Clone, Copy)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Delete,
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
            HttpMethod::Delete => Method::DELETE,
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
            HttpMethod::Delete => router.delete(&converted_path, self.handler),
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
/// Per-route middleware can be chained via `.middleware(M)`:
///
/// ```rust,ignore
/// ws!("/ws/private", PrivateHandler).middleware(SessionMiddleware::new())
/// ```
///
/// The chain runs over the upgrade `Request` before the handler is
/// dispatched; a non-2xx response short-circuits the upgrade.
pub struct WsRouteDef {
    path: &'static str,
    handler: crate::ws::BoxedWebSocketHandler,
    middleware: Vec<BoxedMiddleware>,
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

    /// Register this WS route on the given `Router`. Called by the
    /// `routes!` macro's expansion; not intended for direct use.
    pub fn register(self, router: Router) -> Router {
        router.ws_boxed_with_middleware(self.path, self.handler, self.middleware)
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
    ($handler:expr) => {{
        $crate::__fallback_impl($handler)
    }};
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

/// An item that can be added to a route group - either a route or a nested group
pub enum GroupItem {
    /// A single route
    Route(GroupRoute),
    /// A nested group with its own prefix and middleware
    NestedGroup(Box<GroupDef>),
}

/// Trait for types that can be converted into a GroupItem
pub trait IntoGroupItem {
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
    /// - If route path is "/" (root), the full path is just the group prefix
    /// - Otherwise, prefix and route path are concatenated
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
        // Build the full prefix for this group
        let full_prefix = if parent_prefix.is_empty() {
            self.prefix.to_string()
        } else {
            format!("{}{}", parent_prefix, self.prefix)
        };

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
                    // Convert :param to {param} for matchit compatibility
                    let converted_route_path = convert_route_params(route.path);

                    // Build full path with prefix
                    // If route path is "/" (root), just use the prefix without trailing slash
                    let full_path = if converted_route_path == "/" {
                        full_prefix.clone()
                    } else {
                        format!("{}{}", full_prefix, converted_route_path)
                    };
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
                        HttpMethod::Delete => {
                            router.insert_delete(full_path, route.handler);
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
        assert_eq!(convert_route_params("/api/v1/:version"), "/api/v1/{version}");
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
        let group = GroupDef::__new_unchecked("/api")
            .add(RouteDefBuilder::new(HttpMethod::Get, "/users", test_handler));

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
            .add(RouteDefBuilder::new(HttpMethod::Get, "/users", test_handler))
            .add(nested)
            .add(RouteDefBuilder::new(HttpMethod::Post, "/users", test_handler));

        assert_eq!(group.items.len(), 3);
        matches!(&group.items[0], GroupItem::Route(_));
        matches!(&group.items[1], GroupItem::NestedGroup(_));
        matches!(&group.items[2], GroupItem::Route(_));
    }

    #[test]
    fn test_deep_nesting() {
        // Test deeply nested groups (3 levels)
        let level3 = GroupDef::__new_unchecked("/level3")
            .add(RouteDefBuilder::new(HttpMethod::Get, "/", test_handler));

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
        let group = GroupDef::__new_unchecked("/api")
            .route(RouteDefBuilder::new(HttpMethod::Get, "/users", test_handler));

        assert_eq!(group.items.len(), 1);
        matches!(&group.items[0], GroupItem::Route(_));
    }
}
