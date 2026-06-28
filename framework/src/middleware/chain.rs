//! Middleware chain execution engine

use super::{BoxedMiddleware, MiddlewareFuture, Next};
use crate::http::{Request, Response};
use crate::routing::BoxedHandler;
use std::sync::Arc;

/// Builds and executes the middleware chain
///
/// The chain runs from the outside in:
/// 1. Global middleware (first to run)
/// 2. Route-level middleware. Route-*group* middleware is not a distinct
///    runtime layer: group finalization flattens it into each grouped
///    route's `(method, pattern)` middleware (see
///    `routing::group::GroupBuilder::try_finalize`), so by execution time
///    it is indistinguishable from middleware attached to the route
///    directly. Runtime behavior is correct — group middleware still runs
///    ahead of route middleware because it is registered first — but
///    introspection cannot tell group from route middleware apart.
/// 3. The actual route handler (innermost)
pub struct MiddlewareChain {
    middleware: Vec<BoxedMiddleware>,
}

impl MiddlewareChain {
    /// Create a new empty middleware chain
    pub fn new() -> Self {
        Self {
            middleware: Vec::new(),
        }
    }

    /// Create a new middleware chain with enough pre-allocated capacity to
    /// hold `n` entries without re-allocating as middleware is pushed or
    /// extended into it.
    ///
    /// The request hot path builds a chain of `1 (RequestId) + global.len() + route.len()`
    /// entries on every request. Using [`Self::new()`] there forces the backing `Vec`
    /// to grow (and copy) two-to-three times as the chain is assembled via `push` then
    /// `extend` then `extend`. Pre-sizing to the known total collapses that to a single
    /// allocation, eliminating per-request re-allocation overhead.
    pub fn with_capacity(n: usize) -> Self {
        Self {
            middleware: Vec::with_capacity(n),
        }
    }

    /// Add middleware to the chain
    ///
    /// Middleware are executed in the order they are added.
    pub fn push(&mut self, middleware: BoxedMiddleware) {
        self.middleware.push(middleware);
    }

    /// Add multiple middleware to the chain
    pub fn extend(&mut self, middleware: impl IntoIterator<Item = BoxedMiddleware>) {
        self.middleware.extend(middleware);
    }

    /// Execute the middleware chain with the given request and final handler
    ///
    /// The chain is executed from outside-in:
    /// - First middleware added runs first
    /// - Each middleware can call `next(request)` to continue the chain
    /// - The final handler is called at the end of the chain
    ///
    /// # Panics
    ///
    /// This composition primitive does NOT catch panics. A panic in any
    /// middleware or in the handler unwinds straight out of `execute`,
    /// exactly like any other async fn. The request-path safety net that
    /// converts a panic into a sanitized 500 (plus the structured log and
    /// the `ErrorOccurred` dispatch) lives at the server boundary —
    /// `server::execute_chain_safely` for HTTP, `handle_ws_upgrade` for
    /// WebSocket upgrades — so that standardized handling happens exactly
    /// once, where the request lifecycle owns it, rather than being
    /// duplicated inside this layer-agnostic primitive. The end-to-end
    /// behavior is locked by `tests/middleware_panic_safety.rs`. A consumer
    /// driving a chain outside that boundary is responsible for its own
    /// `catch_unwind` if it wants the same guarantee.
    pub async fn execute(self, request: Request, handler: Arc<BoxedHandler>) -> Response {
        if self.middleware.is_empty() {
            // No middleware - call handler directly
            return handler(request).await;
        }

        // Build the chain from inside-out
        // Start with the actual handler as the innermost "next"
        let handler_clone = handler.clone();
        let mut next: Next = Arc::new(move |req| handler_clone(req));

        // Wrap each middleware around the next, from last to first
        // This creates the correct execution order: first middleware runs first
        for middleware in self.middleware.into_iter().rev() {
            let current_next = next;
            let mw = middleware;
            next = Arc::new(move |req| {
                let n = current_next.clone();
                let m = mw.clone();
                Box::pin(async move { m(req, n).await }) as MiddlewareFuture
            });
        }

        // Execute the outermost middleware (which was the first added)
        next(request).await
    }
}

impl Default for MiddlewareChain {
    fn default() -> Self {
        Self::new()
    }
}
