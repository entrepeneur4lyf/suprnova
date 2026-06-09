//! Route grouping with shared prefix and middleware

use super::macros::{convert_route_params, join_paths};
use super::{BoxedHandler, RouteBuilder, Router};
use crate::FrameworkError;
use crate::http::{Request, Response};
use crate::middleware::{BoxedMiddleware, Middleware, into_boxed};
use hyper::Method;
use std::future::Future;
use std::sync::Arc;

/// Builder for route groups with shared prefix and middleware
///
/// # Example
///
/// ```rust,ignore
/// Router::new()
///     .group("/api", |r| {
///         r.get("/users", list_users)
///          .post("/users", create_user)
///     }).middleware(ApiMiddleware)
/// ```
pub struct GroupBuilder {
    /// The outer router we're building into
    outer_router: Router,
    /// Routes registered within this group (stored as full paths)
    group_routes: Vec<GroupRoute>,
    /// The prefix for this group
    prefix: String,
    /// Middleware to apply to all routes in this group
    middleware: Vec<BoxedMiddleware>,
}

/// A route registered within a group
struct GroupRoute {
    method: GroupMethod,
    path: String,
    handler: Arc<BoxedHandler>,
}

#[derive(Clone, Copy)]
enum GroupMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl GroupBuilder {
    /// Apply middleware to all routes in this group
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Router::new()
    ///     .group("/api", |r| r.get("/users", handler))
    ///     .middleware(ApiMiddleware)
    /// ```
    pub fn middleware<M: Middleware + 'static>(mut self, middleware: M) -> Self {
        self.middleware.push(into_boxed(middleware));
        self
    }

    /// Finalize the group and merge routes into the outer router.
    ///
    /// # Panics
    ///
    /// Panics on a duplicate or malformed route pattern (same boot-time
    /// fail-loud policy as [`Router::get`]). This is the engine behind
    /// `Router::from(group_builder)` / `group_builder.into()`. Use
    /// [`GroupBuilder::try_finalize`] for a fallible variant that returns
    /// `Err(FrameworkError)` instead of panicking.
    fn finalize(self) -> Router {
        self.try_finalize().unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of the `From`/`into` conversion: merge the group's
    /// routes into the outer router, returning `Err(FrameworkError)`
    /// (naming the offending method + full path) on a duplicate or
    /// malformed pattern instead of panicking.
    ///
    /// A manual `TryFrom<GroupBuilder> for Router` is impossible — the
    /// existing `From<GroupBuilder> for Router` triggers the std blanket
    /// `impl<T, U: Into<T>> TryFrom<U> for T` (with `Error = Infallible`),
    /// so a second `TryFrom` impl would be a conflicting implementation.
    /// This inherent method is the idiomatic fallible entry point; prefer
    /// it when group prefixes or inner paths come from a source you don't
    /// control at compile time.
    ///
    /// Path normalisation: prefix + inner path are joined on a single
    /// canonical `/` boundary (`join_paths` — a child of `/` resolves to
    /// the prefix itself, a root `/` prefix contributes nothing) and then
    /// run through `convert_route_params` so Express-style `:id` segments
    /// are translated to matchit-style `{id}`. The same canonical pattern
    /// is used both for the matchit insert and for the middleware lookup
    /// key — without that, group middleware on a parameterised route
    /// would miss the dispatcher's lookup (it queries by matched pattern,
    /// not raw path).
    pub fn try_finalize(mut self) -> Result<Router, FrameworkError> {
        // Insert all group routes into the outer router with the prefix
        for route in self.group_routes {
            let raw_full = join_paths(&self.prefix, &route.path);
            let full_path = convert_route_params(&raw_full);

            // Insert into the appropriate method router using pub(crate)
            // fallible methods, and capture the canonical `hyper::Method` so
            // middleware is keyed by (method, path) — sibling routes on the
            // same path under different methods MUST NOT share middleware.
            let http_method = match route.method {
                GroupMethod::Get => {
                    self.outer_router
                        .try_insert_get(&full_path, route.handler)?;
                    Method::GET
                }
                GroupMethod::Post => {
                    self.outer_router
                        .try_insert_post(&full_path, route.handler)?;
                    Method::POST
                }
                GroupMethod::Put => {
                    self.outer_router
                        .try_insert_put(&full_path, route.handler)?;
                    Method::PUT
                }
                GroupMethod::Patch => {
                    self.outer_router
                        .try_insert_patch(&full_path, route.handler)?;
                    Method::PATCH
                }
                GroupMethod::Delete => {
                    self.outer_router
                        .try_insert_delete(&full_path, route.handler)?;
                    Method::DELETE
                }
                GroupMethod::Head => {
                    self.outer_router
                        .try_insert_head(&full_path, route.handler)?;
                    Method::HEAD
                }
                GroupMethod::Options => {
                    self.outer_router
                        .try_insert_options(&full_path, route.handler)?;
                    Method::OPTIONS
                }
            };

            // Apply group middleware to each route under its own
            // (method, converted_path) key. The dispatcher in
            // `server.rs` looks middleware up by the matched pattern;
            // both insert and lookup must therefore use the same
            // canonical form.
            for mw in &self.middleware {
                self.outer_router
                    .add_middleware(http_method.clone(), &full_path, mw.clone());
            }
        }

        Ok(self.outer_router)
    }
}

/// Inner router used within a group closure
///
/// This captures routes without a prefix, which are later merged with the group's prefix.
pub struct GroupRouter {
    routes: Vec<GroupRoute>,
}

impl GroupRouter {
    fn new() -> Self {
        Self { routes: Vec::new() }
    }

    /// Register a GET route within the group
    pub fn get<H, Fut>(mut self, path: &str, handler: H) -> Self
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.routes.push(GroupRoute {
            method: GroupMethod::Get,
            path: path.to_string(),
            handler: Arc::new(boxed),
        });
        self
    }

    /// Register a POST route within the group
    pub fn post<H, Fut>(mut self, path: &str, handler: H) -> Self
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.routes.push(GroupRoute {
            method: GroupMethod::Post,
            path: path.to_string(),
            handler: Arc::new(boxed),
        });
        self
    }

    /// Register a PUT route within the group
    pub fn put<H, Fut>(mut self, path: &str, handler: H) -> Self
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.routes.push(GroupRoute {
            method: GroupMethod::Put,
            path: path.to_string(),
            handler: Arc::new(boxed),
        });
        self
    }

    /// Register a DELETE route within the group
    pub fn delete<H, Fut>(mut self, path: &str, handler: H) -> Self
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.routes.push(GroupRoute {
            method: GroupMethod::Delete,
            path: path.to_string(),
            handler: Arc::new(boxed),
        });
        self
    }

    /// Register a PATCH route within the group.
    pub fn patch<H, Fut>(mut self, path: &str, handler: H) -> Self
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.routes.push(GroupRoute {
            method: GroupMethod::Patch,
            path: path.to_string(),
            handler: Arc::new(boxed),
        });
        self
    }

    /// Register a HEAD route within the group.
    ///
    /// As elsewhere, HEAD requests fall back to GET when no explicit
    /// HEAD route is registered (RFC 9110 §9.3.2); this method is for
    /// the explicit-override case (e.g. returning custom headers
    /// without running the GET body computation).
    pub fn head<H, Fut>(mut self, path: &str, handler: H) -> Self
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.routes.push(GroupRoute {
            method: GroupMethod::Head,
            path: path.to_string(),
            handler: Arc::new(boxed),
        });
        self
    }

    /// Register an OPTIONS route within the group.
    ///
    /// CORS preflight is handled by `CorsMiddleware` at the
    /// global-middleware layer; this method serves non-preflight uses
    /// (allowed-verb discovery etc.).
    pub fn options<H, Fut>(mut self, path: &str, handler: H) -> Self
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        self.routes.push(GroupRoute {
            method: GroupMethod::Options,
            path: path.to_string(),
            handler: Arc::new(boxed),
        });
        self
    }

    /// Register one handler against every common HTTP method
    /// (GET / POST / PUT / PATCH / DELETE / HEAD / OPTIONS).
    ///
    /// The same boxed handler is shared (cloned `Arc`) across all
    /// seven method-routes within the group. Group middleware applied
    /// via [`GroupBuilder::middleware`] fans across every method at
    /// finalize time, matching the fluent `Router::any` fan-out
    /// semantics.
    pub fn any<H, Fut>(self, path: &str, handler: H) -> Self
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        let arc = Arc::new(boxed);
        self.push_routes_for_methods(
            path,
            [
                GroupMethod::Get,
                GroupMethod::Post,
                GroupMethod::Put,
                GroupMethod::Patch,
                GroupMethod::Delete,
                GroupMethod::Head,
                GroupMethod::Options,
            ],
            arc,
        )
    }

    /// Register one handler against an explicit list of HTTP methods —
    /// Laravel `Route::match([...], ...)` in the fluent-group form.
    ///
    /// # Panics
    ///
    /// Panics if `methods` is empty or contains a verb other than
    /// GET / POST / PUT / PATCH / DELETE / HEAD / OPTIONS. Use
    /// [`GroupRouter::try_methods`] for a fallible sibling that
    /// returns `Err(FrameworkError)` instead — the right choice when
    /// the method list comes from a config file or other runtime
    /// source you can't validate at compile time.
    pub fn methods<H, Fut>(self, methods: &[Method], path: &str, handler: H) -> Self
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.try_methods(methods, path, handler)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// Fallible sibling of [`GroupRouter::methods`]: returns
    /// `Err(FrameworkError)` on an empty method slice or unsupported
    /// verb instead of panicking. Preferred when the method list is
    /// dynamic (config-driven, OpenAPI-derived, etc.).
    pub fn try_methods<H, Fut>(
        self,
        methods: &[Method],
        path: &str,
        handler: H,
    ) -> Result<Self, FrameworkError>
    where
        H: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        if methods.is_empty() {
            return Err(FrameworkError::internal(
                "GroupBuilder::methods() requires at least one HTTP method",
            ));
        }
        let mut group_methods = Vec::with_capacity(methods.len());
        for m in methods {
            let gm = match *m {
                Method::GET => GroupMethod::Get,
                Method::POST => GroupMethod::Post,
                Method::PUT => GroupMethod::Put,
                Method::PATCH => GroupMethod::Patch,
                Method::DELETE => GroupMethod::Delete,
                Method::HEAD => GroupMethod::Head,
                Method::OPTIONS => GroupMethod::Options,
                ref other => {
                    return Err(FrameworkError::internal(format!(
                        "GroupBuilder::methods() got unsupported HTTP method '{other}'; only \
                         GET/POST/PUT/PATCH/DELETE/HEAD/OPTIONS are accepted",
                    )));
                }
            };
            group_methods.push(gm);
        }
        let boxed: BoxedHandler = Box::new(move |req| Box::pin(handler(req)));
        let arc = Arc::new(boxed);
        Ok(self.push_routes_for_methods(path, group_methods, arc))
    }

    /// Internal helper used by [`GroupRouter::any`] and
    /// [`GroupRouter::methods`]. Pushes one `GroupRoute` entry per
    /// requested method, all sharing the same `Arc<BoxedHandler>` so
    /// per-method dispatch stays O(1) at finalize time.
    fn push_routes_for_methods(
        mut self,
        path: &str,
        methods: impl IntoIterator<Item = GroupMethod>,
        handler: Arc<BoxedHandler>,
    ) -> Self {
        for method in methods {
            self.routes.push(GroupRoute {
                method,
                path: path.to_string(),
                handler: handler.clone(),
            });
        }
        self
    }
}

impl Router {
    /// Create a route group with a shared prefix
    ///
    /// Routes defined within the group will have the prefix prepended to their paths.
    /// Middleware applied to the group will be applied to all routes within it.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Router::new()
    ///     .group("/api", |r| {
    ///         r.get("/users", list_users)      // -> GET /api/users
    ///          .post("/users", create_user)    // -> POST /api/users
    ///          .get("/users/{id}", show_user)  // -> GET /api/users/{id}
    ///     })
    ///     .middleware(ApiMiddleware)
    /// ```
    pub fn group<F>(self, prefix: &str, builder_fn: F) -> GroupBuilder
    where
        F: FnOnce(GroupRouter) -> GroupRouter,
    {
        let inner = GroupRouter::new();
        let built = builder_fn(inner);

        GroupBuilder {
            outer_router: self,
            group_routes: built.routes,
            prefix: prefix.to_string(),
            middleware: Vec::new(),
        }
    }
}

impl From<GroupBuilder> for Router {
    fn from(builder: GroupBuilder) -> Self {
        builder.finalize()
    }
}

// Allow RouteBuilder to chain into groups
impl RouteBuilder {
    /// Create a route group with a shared prefix
    pub fn group<F>(self, prefix: &str, builder_fn: F) -> GroupBuilder
    where
        F: FnOnce(GroupRouter) -> GroupRouter,
    {
        self.router.group(prefix, builder_fn)
    }
}

#[cfg(test)]
mod tests {
    //! Fluent GroupRouter parity tests.

    use super::*;
    use crate::http::text;
    use hyper::Method;

    async fn h(_req: Request) -> Response {
        text("ok")
    }

    /// Fluent `r.patch(...)` inside `router.group(...)` registers
    /// PATCH at `prefix + path`, mirroring the macro-group arm.
    #[test]
    fn fluent_group_registers_patch() {
        let router: Router = Router::new()
            .group("/api", |r| r.patch("/users/:id", h))
            .into();
        assert!(
            router
                .match_route(&Method::PATCH, "/api/users/42")
                .is_some()
        );
    }

    /// Fluent `r.head(...)` registers explicit HEAD (the GET fallback
    /// already runs without this — explicit HEAD is for the override
    /// case).
    #[test]
    fn fluent_group_registers_head_explicit() {
        let router: Router = Router::new().group("/probes", |r| r.head("/x", h)).into();
        assert!(router.has_explicit_head("/probes/x"));
    }

    /// Fluent `r.options(...)` registers OPTIONS.
    #[test]
    fn fluent_group_registers_options() {
        let router: Router = Router::new()
            .group("/api", |r| r.options("/discover", h))
            .into();
        assert!(
            router
                .match_route(&Method::OPTIONS, "/api/discover")
                .is_some()
        );
    }

    /// Fluent `r.any(...)` fans the handler across all seven common
    /// HTTP methods. Pins per-method matching at finalize time after
    /// prefix concatenation.
    #[test]
    fn fluent_group_any_registers_all_methods() {
        let router: Router = Router::new().group("/api", |r| r.any("/webhook", h)).into();
        for m in [
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::HEAD,
            Method::OPTIONS,
        ] {
            assert!(
                router.match_route(&m, "/api/webhook").is_some(),
                "fluent group any() must register {m} at /api/webhook",
            );
        }
    }

    /// Fluent `r.methods(&[GET, HEAD])` registers exactly those verbs.
    #[test]
    fn fluent_group_methods_registers_requested_only() {
        let router: Router = Router::new()
            .group("/api", |r| {
                r.methods(&[Method::GET, Method::HEAD], "/probes", h)
            })
            .into();
        assert!(router.match_route(&Method::GET, "/api/probes").is_some());
        assert!(router.has_explicit_head("/api/probes"));
        assert!(router.match_route(&Method::POST, "/api/probes").is_none());
    }

    /// Group-level middleware applied via `.middleware(M)` fans across
    /// every verb of a fluent `any()` route. Mirrors the macro-group
    /// fan-out, closing the audit MEDIUM about fluent vs macro group
    /// disparity.
    #[test]
    fn fluent_group_middleware_fans_across_any_methods() {
        use crate::middleware::{Middleware, Next};
        use async_trait::async_trait;

        #[derive(Clone)]
        struct NoopMw;
        #[async_trait]
        impl Middleware for NoopMw {
            async fn handle(&self, request: Request, next: Next) -> Response {
                next(request).await
            }
        }

        let router: Router = Router::new()
            .group("/api", |r| r.any("/wh", h))
            .middleware(NoopMw)
            .into();

        for m in [
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::HEAD,
            Method::OPTIONS,
        ] {
            assert_eq!(
                router.get_route_middleware(&m, "/api/wh").len(),
                1,
                "group middleware must reach the any-route for {m}",
            );
        }
    }

    /// Empty methods list panics with a useful message.
    #[test]
    #[should_panic(expected = "at least one HTTP method")]
    fn fluent_group_methods_empty_list_panics() {
        let _ = Router::new()
            .group("/api", |r| r.methods(&[], "/x", h))
            .finalize();
    }

    /// Unsupported HTTP method (CONNECT, TRACE, etc.) panics naming
    /// the offender.
    #[test]
    #[should_panic(expected = "CONNECT")]
    fn fluent_group_methods_rejects_unsupported_verb() {
        let bad = Method::from_bytes(b"CONNECT").expect("valid CONNECT");
        let _ = Router::new()
            .group("/api", |r| r.methods(&[Method::GET, bad], "/x", h))
            .finalize();
    }

    /// Child path `/` inside a fluent group registers the group prefix
    /// itself, not `prefix + "/"`. Mirrors the macro-group special case
    /// so visually-equivalent macro and fluent definitions produce the
    /// same route table.
    #[test]
    fn fluent_group_root_child_path_collapses_to_prefix() {
        let router: Router = Router::new().group("/api", |r| r.get("/", h)).into();
        assert!(
            router.match_route(&Method::GET, "/api").is_some(),
            "fluent group with child path '/' must register at the group prefix",
        );
        assert!(
            router.match_route(&Method::GET, "/api/").is_none(),
            "fluent group with child path '/' must NOT register at prefix + '/'",
        );
    }

    /// Non-root child paths still concatenate normally.
    #[test]
    fn fluent_group_non_root_child_path_concatenates() {
        let router: Router = Router::new().group("/api", |r| r.get("/users", h)).into();
        assert!(router.match_route(&Method::GET, "/api/users").is_some());
        assert!(router.match_route(&Method::GET, "/api").is_none());
    }

    /// A root-prefix fluent group (`.group("/", …)`) registers child
    /// paths verbatim — never `//login`-style unmatchable patterns.
    /// Mirrors the macro-group regression found by the Nebula kit.
    #[test]
    fn fluent_root_prefix_group_routes_match() {
        let router: Router = Router::new().group("/", |r| r.get("/login", h)).into();
        assert!(router.match_route(&Method::GET, "/login").is_some());
        assert!(router.match_route(&Method::GET, "//login").is_none());
    }

    /// Group middleware on a root-prefix group is keyed by the same
    /// canonical path the route was inserted under, so it actually
    /// fires for `/login`.
    #[test]
    fn fluent_root_prefix_group_middleware_reaches_routes() {
        use crate::middleware::{Middleware, Next};
        use async_trait::async_trait;

        #[derive(Clone)]
        struct NoopMw;
        #[async_trait]
        impl Middleware for NoopMw {
            async fn handle(&self, request: Request, next: Next) -> Response {
                next(request).await
            }
        }

        let router: Router = Router::new()
            .group("/", |r| r.get("/login", h))
            .middleware(NoopMw)
            .into();
        assert_eq!(router.get_route_middleware(&Method::GET, "/login").len(), 1,);
    }

    /// Trailing-slash prefixes join cleanly on the fluent surface too.
    #[test]
    fn fluent_trailing_slash_prefix_joins_cleanly() {
        let router: Router = Router::new().group("/api/", |r| r.get("/users", h)).into();
        assert!(router.match_route(&Method::GET, "/api/users").is_some());
    }
}
