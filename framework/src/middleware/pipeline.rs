//! Laravel-shape fluent pipeline over `MiddlewareChain`.
//!
//! `Pipeline` is the Suprnova analogue of `Illuminate\Pipeline\Pipeline`.
//! Where Laravel writes
//!
//! ```php
//! (new Pipeline($container))
//!     ->send($request)
//!     ->through([Auth::class, Cors::class])
//!     ->then(fn ($req) => $router->dispatch($req));
//! ```
//!
//! Suprnova writes
//!
//! ```rust,ignore
//! Pipeline::new()
//!     .send(request)
//!     .through([Arc::new(AuthMw), Arc::new(CorsMw)])
//!     .then(|req| async move { handler(req).await })
//!     .await;
//! ```
//!
//! The pipeline is a thin owned-state wrapper around [`MiddlewareChain`] —
//! the chain remains the execution primitive, the pipeline adds the
//! ergonomic builder Laravel users reach for. `then`, `then_return`, and
//! `finally_with` all funnel through `MiddlewareChain::execute`.

use super::{BoxedMiddleware, Middleware, MiddlewareChain, into_boxed};
use crate::http::{Request, Response};
use crate::routing::BoxedHandler;
use std::future::Future;
use std::sync::Arc;

/// Builder-style fluent pipeline. Mirrors `Illuminate\Pipeline\Pipeline`.
///
/// The pipeline accumulates a list of middleware via [`Pipeline::through`]
/// / [`Pipeline::pipe`], an optional `passable` request via
/// [`Pipeline::send`], and an optional `finally` callback via
/// [`Pipeline::finally_with`]. Calling [`Pipeline::then`] executes the
/// chain and runs the destination.
///
/// # Dual API
///
/// The Laravel-side names (`send`, `through`, `pipe`, `then`, `then_return`,
/// `finally_with`) are first-class. Rust-side aliases (`with_request`,
/// `with_middleware`, `push`, `execute`, `on_finally`) ship for callers who
/// prefer them — see the impl block.
pub struct Pipeline {
    /// The list of middleware in execution order (first added runs first).
    pipes: Vec<BoxedMiddleware>,
    /// The request being threaded through the pipeline. `None` means the
    /// caller must pass it directly to [`Pipeline::then`] via the
    /// destination closure or build the request later via [`Pipeline::send`].
    passable: Option<Request>,
    /// Optional `finally` hook. Runs after the destination completes,
    /// regardless of how the chain resolved (no panic-catching — see
    /// [`MiddlewareChain::execute`] for the panic policy).
    finally: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Pipeline {
    /// Construct an empty pipeline. Equivalent to Laravel's
    /// `new Pipeline($container)`; the container parameter is absent here
    /// because Suprnova resolves middleware through the trait system, not
    /// reflection.
    pub fn new() -> Self {
        Self {
            pipes: Vec::new(),
            passable: None,
            finally: None,
        }
    }

    /// Set the object being sent through the pipeline. Laravel's `send`.
    pub fn send(mut self, passable: Request) -> Self {
        self.passable = Some(passable);
        self
    }

    /// Rust-side alias for [`Pipeline::send`].
    pub fn with_request(self, request: Request) -> Self {
        self.send(request)
    }

    /// Set the list of pipes (replacing anything already accumulated).
    /// Laravel's `through`.
    pub fn through<I, M>(mut self, pipes: I) -> Self
    where
        I: IntoIterator<Item = M>,
        M: Middleware + 'static,
    {
        self.pipes = pipes.into_iter().map(into_boxed).collect();
        self
    }

    /// Set the list of pipes from a sequence of pre-boxed middleware. The
    /// non-generic flavour callers reach for when they already have a
    /// `Vec<BoxedMiddleware>` (e.g. from a registry snapshot).
    pub fn through_boxed<I>(mut self, pipes: I) -> Self
    where
        I: IntoIterator<Item = BoxedMiddleware>,
    {
        self.pipes = pipes.into_iter().collect();
        self
    }

    /// Rust-side alias for [`Pipeline::through`].
    pub fn with_middleware<I, M>(self, middleware: I) -> Self
    where
        I: IntoIterator<Item = M>,
        M: Middleware + 'static,
    {
        self.through(middleware)
    }

    /// Append additional pipes. Laravel's `pipe`. Where `through` REPLACES
    /// the list, `pipe` PUSHES onto whatever is already there.
    pub fn pipe<M: Middleware + 'static>(mut self, middleware: M) -> Self {
        self.pipes.push(into_boxed(middleware));
        self
    }

    /// Append a pre-boxed middleware. Companion to [`Pipeline::through_boxed`].
    pub fn pipe_boxed(mut self, middleware: BoxedMiddleware) -> Self {
        self.pipes.push(middleware);
        self
    }

    /// Rust-side alias for [`Pipeline::pipe`].
    pub fn push<M: Middleware + 'static>(self, middleware: M) -> Self {
        self.pipe(middleware)
    }

    /// Register a `finally` callback that runs after the destination
    /// resolves. Mirrors Laravel's `finally`. Named `finally_with` (not
    /// `finally`) because `finally` is a reserved keyword in some target
    /// languages and we want the Suprnova surface to be safe to call from
    /// macro-generated code unconditionally.
    pub fn finally_with<F>(mut self, callback: F) -> Self
    where
        F: FnOnce() + Send + Sync + 'static,
    {
        self.finally = Some(Box::new(callback));
        self
    }

    /// Rust-side alias for [`Pipeline::finally_with`].
    pub fn on_finally<F>(self, callback: F) -> Self
    where
        F: FnOnce() + Send + Sync + 'static,
    {
        self.finally_with(callback)
    }

    /// Run the pipeline with a final destination handler. The handler
    /// receives the request and returns a [`Response`].
    ///
    /// Mirrors Laravel's `then($destination)`. Consumes `self`.
    ///
    /// # Panics
    ///
    /// Panics if the pipeline was constructed without a request (no
    /// [`Pipeline::send`] call). Use [`Pipeline::then_with`] to pass the
    /// request inline as a single call, or [`Pipeline::try_then`] for
    /// a fallible variant that returns `Err` instead of panicking.
    pub async fn then<F, Fut>(mut self, destination: F) -> Response
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let request = self
            .passable
            .take()
            .expect("Pipeline::then requires a request — call .send(request) first");
        self.then_with(request, destination).await
    }

    /// Fallible sibling of [`Self::then`] — returns
    /// `Err(FrameworkError::internal(...))` instead of panicking when
    /// the pipeline was assembled without a [`Pipeline::send`] call.
    ///
    /// Prefer this from queue-worker, scheduler, and middleware-builder
    /// utility code where a panic would tear down the surrounding
    /// task. The success path is otherwise identical to [`Self::then`].
    pub async fn try_then<F, Fut>(
        mut self,
        destination: F,
    ) -> Result<Response, crate::FrameworkError>
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let request = self.passable.take().ok_or_else(|| {
            crate::FrameworkError::internal(
                "Pipeline::try_then requires a request — call .send(request) first, \
                 or use Pipeline::then_with to pass the request inline",
            )
        })?;
        Ok(self.then_with(request, destination).await)
    }

    /// Run the pipeline against an explicit request, ignoring any
    /// previously-set passable. Convenience for callers that build the
    /// request after the chain.
    pub async fn then_with<F, Fut>(self, request: Request, destination: F) -> Response
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let handler: BoxedHandler = Box::new(move |req| Box::pin(destination(req)));
        let response = self.execute_chain(request, Arc::new(handler)).await;
        if let Some(cb) = self.finally {
            cb();
        }
        response
    }

    /// Run the pipeline with the request as the final value (Laravel's
    /// `thenReturn`). The terminal step echoes the request back as the
    /// pipeline result — useful when the pipeline is used purely for
    /// side effects.
    ///
    /// Because Suprnova pipelines yield a [`Response`] (the only thing
    /// HTTP handlers can return), this surface materialises a 204
    /// No Content response after the chain runs. The "thenReturn"
    /// semantics are preserved at the side-effect level — every
    /// middleware ran, every `finally` fired — without forcing the
    /// caller to invent a destination.
    pub async fn then_return(self) -> Response {
        self.then(|_req| async { Ok(crate::http::HttpResponse::new().status(204)) })
            .await
    }

    /// Rust-side alias for [`Pipeline::then`]. Plays nicely with
    /// `.await execute(...)` reading patterns.
    pub async fn execute<F, Fut>(self, destination: F) -> Response
    where
        F: Fn(Request) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        self.then(destination).await
    }

    /// Number of pipes currently registered. Useful for tests and
    /// introspection.
    pub fn len(&self) -> usize {
        self.pipes.len()
    }

    /// Whether no pipes are registered.
    pub fn is_empty(&self) -> bool {
        self.pipes.is_empty()
    }

    /// Borrow the boxed pipes (for tests / introspection).
    pub fn pipes(&self) -> &[BoxedMiddleware] {
        &self.pipes
    }

    /// Internal: actually run the chain. The split exists so `then`,
    /// `then_with`, and `then_return` all go through one execution path.
    async fn execute_chain(&self, request: Request, handler: Arc<BoxedHandler>) -> Response {
        let mut chain = MiddlewareChain::new();
        for mw in &self.pipes {
            chain.push(mw.clone());
        }
        chain.execute(request, handler).await
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct NoopMw;
    #[async_trait]
    impl Middleware for NoopMw {
        async fn handle(&self, request: Request, next: super::super::Next) -> Response {
            next(request).await
        }
    }

    /// `through` REPLACES the current pipe list; `pipe` APPENDS to it.
    /// Pins the Laravel-shape semantics of both surfaces.
    #[test]
    fn through_replaces_pipe_appends() {
        let p = Pipeline::new()
            .through([NoopMw, NoopMw])
            .pipe(NoopMw)
            .pipe(NoopMw);
        assert_eq!(p.len(), 4);

        // through replaces — even after pipes have been pushed
        let p = p.through([NoopMw]);
        assert_eq!(p.len(), 1);
    }

    /// Rust-side aliases (`with_middleware`, `push`) compile and behave
    /// identically to their Laravel counterparts (`through`, `pipe`).
    #[test]
    fn rust_side_aliases_match_laravel_shape() {
        let p = Pipeline::new()
            .with_middleware([NoopMw, NoopMw])
            .push(NoopMw);
        assert_eq!(p.len(), 3);
    }

    /// `through_boxed` / `pipe_boxed` accept pre-boxed middleware. The
    /// non-generic flavours exist so registry snapshots
    /// (`Vec<BoxedMiddleware>`) can be threaded into a pipeline without
    /// re-wrapping every entry.
    #[test]
    fn boxed_inputs_thread_through_without_rewrap() {
        let mw: BoxedMiddleware = into_boxed(NoopMw);
        let p = Pipeline::new()
            .through_boxed([mw.clone()])
            .pipe_boxed(mw.clone());
        assert_eq!(p.len(), 2);
    }

    /// New pipeline starts empty.
    #[test]
    fn new_pipeline_is_empty() {
        let p = Pipeline::new();
        assert!(p.is_empty());
        assert_eq!(p.len(), 0);
        assert!(p.pipes().is_empty());
    }

    /// `try_then` returns `Err(FrameworkError::internal(...))` instead
    /// of panicking when the pipeline was assembled without a
    /// `send(request)` call. Mirrors the documented `then` panic shape
    /// at the `Result` boundary for queue-worker / scheduler /
    /// middleware-builder callers that can't afford a process panic.
    #[tokio::test]
    async fn try_then_returns_err_without_send() {
        let p = Pipeline::new().pipe(NoopMw);
        let result = p
            .try_then(|_req| async { Ok(crate::http::HttpResponse::text("never reached")) })
            .await;
        assert!(result.is_err(), "missing send must surface as Err");
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("send"),
            "diagnostic should mention `send`: {msg}"
        );
    }
}
