//! Middleware system for suprnova framework
//!
//! This module provides Laravel 12.x-style middleware support with:
//! - Global middleware (runs on all routes)
//! - Route group middleware (shared for a group of routes)
//! - Per-route middleware (applied to individual routes)
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::{async_trait, Middleware, Next, Request, Response, HttpResponse};
//!
//! pub struct AuthMiddleware;
//!
//! #[async_trait]
//! impl Middleware for AuthMiddleware {
//!     async fn handle(&self, request: Request, next: Next) -> Response {
//!         if request.header("Authorization").is_none() {
//!             return Err(HttpResponse::text("Unauthorized").status(401));
//!         }
//!         next(request).await
//!     }
//! }
//! ```

mod aliases;
mod chain;
mod pipeline;
mod registry;
mod terminable;

pub use aliases::{
    MiddlewareFactory, MiddlewareResolveError, append_middleware_priority, clear_middleware_alias,
    clear_middleware_group, has_middleware_alias, has_middleware_group, middleware_priority,
    prepend_middleware_priority, register_middleware_alias, register_middleware_group,
    registered_middleware_aliases, registered_middleware_groups, resolve_middleware_alias,
    resolve_middleware_group,
};
pub use chain::MiddlewareChain;
pub use pipeline::Pipeline;
pub use registry::{
    MiddlewareRegistry, get_global_middleware, global_middleware_count, has_global_middleware,
    prepend_global_middleware, register_global_middleware,
};
pub use terminable::{
    Terminable, TerminationSnapshot, dispatch_termination, has_terminable, register_terminable,
    registered_terminables, terminable_count,
};

#[doc(hidden)]
pub use aliases::{
    clear_all_middleware_aliases_for_test, clear_all_middleware_groups_for_test,
    clear_middleware_priority_for_test,
};
#[doc(hidden)]
pub use registry::clear_global_middleware_for_test;
#[doc(hidden)]
pub use terminable::clear_terminables_for_test;

use crate::http::{Request, Response};
use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Type alias for the boxed future returned by middleware
pub type MiddlewareFuture = Pin<Box<dyn Future<Output = Response> + Send>>;

/// Type alias for the next handler in the middleware chain
///
/// Call `next(request).await` to pass control to the next middleware or the route handler.
pub type Next = Arc<dyn Fn(Request) -> MiddlewareFuture + Send + Sync>;

/// Type alias for boxed middleware handlers (internal use)
pub type BoxedMiddleware = Arc<dyn Fn(Request, Next) -> MiddlewareFuture + Send + Sync>;

/// Trait for implementing middleware
///
/// Middleware can inspect/modify requests, short-circuit responses, or pass control
/// to the next middleware in the chain by calling `next(request).await`.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{async_trait, Middleware, Next, Request, Response, HttpResponse};
///
/// pub struct LoggingMiddleware;
///
/// #[async_trait]
/// impl Middleware for LoggingMiddleware {
///     async fn handle(&self, request: Request, next: Next) -> Response {
///         println!("--> {} {}", request.method(), request.path());
///         let response = next(request).await;
///         println!("<-- complete");
///         response
///     }
/// }
/// ```
#[async_trait]
pub trait Middleware: Send + Sync {
    /// Handle the request
    ///
    /// - Call `next(request).await` to pass control to the next middleware
    /// - Return `Err(HttpResponse)` to short-circuit and respond immediately
    /// - Modify the response after calling `next()` for post-processing
    async fn handle(&self, request: Request, next: Next) -> Response;
}

/// Convert a Middleware trait object into a BoxedMiddleware
pub fn into_boxed<M: Middleware + 'static>(middleware: M) -> BoxedMiddleware {
    let middleware = Arc::new(middleware);
    Arc::new(move |req, next| {
        let mw = middleware.clone();
        Box::pin(async move { mw.handle(req, next).await })
    })
}
