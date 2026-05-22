//! Route grouping with shared prefix and middleware

use super::macros::convert_route_params;
use super::{BoxedHandler, RouteBuilder, Router};
use crate::http::{Request, Response};
use crate::middleware::{into_boxed, BoxedMiddleware, Middleware};
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
    Delete,
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
    /// Path normalisation: prefix + inner path are concatenated and then
    /// run through `convert_route_params` so Express-style `:id` segments
    /// are translated to matchit-style `{id}`. The same canonical pattern
    /// is used both for the matchit insert and for the middleware lookup
    /// key — without that, group middleware on a parameterised route
    /// would miss the dispatcher's lookup (it queries by matched pattern,
    /// not raw path).
    fn finalize(mut self) -> Router {
        // Insert all group routes into the outer router with the prefix
        for route in self.group_routes {
            let raw_full = format!("{}{}", self.prefix, route.path);
            let full_path = convert_route_params(&raw_full);

            // Insert into the appropriate method router using public(crate) methods,
            // and capture the canonical `hyper::Method` so middleware is keyed by
            // (method, path) — sibling routes on the same path under different
            // methods MUST NOT share middleware.
            let http_method = match route.method {
                GroupMethod::Get => {
                    self.outer_router.insert_get(&full_path, route.handler);
                    Method::GET
                }
                GroupMethod::Post => {
                    self.outer_router.insert_post(&full_path, route.handler);
                    Method::POST
                }
                GroupMethod::Put => {
                    self.outer_router.insert_put(&full_path, route.handler);
                    Method::PUT
                }
                GroupMethod::Delete => {
                    self.outer_router.insert_delete(&full_path, route.handler);
                    Method::DELETE
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

        self.outer_router
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
