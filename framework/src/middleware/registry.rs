//! Middleware registry for global middleware
//!
//! Configure global middleware in `bootstrap.rs` using the `global_middleware!` macro,
//! or use `Server::middleware()` for manual configuration.

use super::{BoxedMiddleware, Middleware, into_boxed};
use std::any::TypeId;
use std::sync::{OnceLock, RwLock};

/// Global middleware registry (populated via `global_middleware!` macro in bootstrap.rs).
///
/// Entries are keyed by the middleware's concrete `TypeId` so registration
/// is idempotent per type — see [`register_global_middleware`].
static GLOBAL_MIDDLEWARE: OnceLock<RwLock<Vec<(TypeId, BoxedMiddleware)>>> = OnceLock::new();

/// Register a global middleware that runs on every request.
///
/// Called by the `global_middleware!` macro. Middleware runs in
/// registration order.
///
/// Registration is **idempotent per middleware type**: registering the
/// same `Middleware` type twice keeps only the first registration. This
/// makes re-running app bootstrap — tests, hot-reload, or constructing
/// more than one `Server` in a process — safe; without it, global
/// logging / auth / CSRF / Inertia middleware would silently double-run
/// on every request. To install several logical instances of the same
/// behavior with different configuration, wrap each in a distinct newtype
/// so they carry distinct `TypeId`s and all register.
///
/// A poisoned write lock — possible if one middleware panicked while
/// another thread held the registry lock during boot — is recovered
/// via `PoisonError::into_inner` rather than silently dropping the
/// registration. Silently failing here would mean global auth / CSRF /
/// logging middleware vanish from every subsequent request based on
/// the outcome of an unrelated panic, which is a security-shaped
/// failure mode.
///
/// # Example
///
/// ```rust,no_run
/// # use suprnova::global_middleware;
/// # use suprnova::middleware::{Middleware, Next};
/// # use suprnova::http::{Request, Response};
/// # use suprnova::async_trait;
/// # #[derive(Clone)] struct LoggingMiddleware;
/// # #[async_trait]
/// # impl Middleware for LoggingMiddleware {
/// #     async fn handle(&self, req: Request, next: Next) -> Response { next(req).await }
/// # }
/// # #[derive(Clone)] struct CorsMiddleware;
/// # #[async_trait]
/// # impl Middleware for CorsMiddleware {
/// #     async fn handle(&self, req: Request, next: Next) -> Response { next(req).await }
/// # }
/// # fn register() {
/// // In bootstrap.rs
/// global_middleware!(LoggingMiddleware);
/// global_middleware!(CorsMiddleware);
/// # }
/// ```
pub fn register_global_middleware<M: Middleware + 'static>(middleware: M) {
    let registry = GLOBAL_MIDDLEWARE.get_or_init(|| RwLock::new(Vec::new()));
    let mut vec = match registry.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    insert_unique_global(&mut vec, middleware);
}

/// Push `middleware` into `registered`, keyed by its concrete type, unless
/// that type is already present. Returns `true` if it was inserted,
/// `false` if a duplicate registration of the same type was skipped.
///
/// Split out from [`register_global_middleware`] so the
/// register-once-per-type contract can be tested against a local vector,
/// independent of the process-global registry.
fn insert_unique_global<M: Middleware + 'static>(
    registered: &mut Vec<(TypeId, BoxedMiddleware)>,
    middleware: M,
) -> bool {
    let type_id = TypeId::of::<M>();
    if registered.iter().any(|(existing, _)| *existing == type_id) {
        tracing::debug!(
            "global middleware of this type is already registered; skipping the \
             duplicate. Wrap it in a distinct newtype to register multiple instances."
        );
        return false;
    }
    registered.push((type_id, into_boxed(middleware)));
    true
}

/// Get all registered global middleware.
///
/// Used internally by `MiddlewareRegistry::from_global()` (and
/// therefore by `Server::from_config()`) to populate the per-request
/// middleware list.
///
/// Poisoned read locks recover via `PoisonError::into_inner` for the
/// same reason as [`register_global_middleware`] — a panic during one
/// global middleware's `handle()` must not silently disable the
/// remaining global middleware on every subsequent request.
pub fn get_global_middleware() -> Vec<BoxedMiddleware> {
    let Some(lock) = GLOBAL_MIDDLEWARE.get() else {
        return Vec::new();
    };
    let guard = match lock.read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.iter().map(|(_, mw)| mw.clone()).collect()
}

/// Prepend a global middleware so it runs **before** every existing
/// global middleware. Mirrors Laravel's `Kernel::prependMiddleware`.
///
/// Registration is still idempotent per concrete type — re-registering
/// an existing global is a no-op. Used by the bootstrap surface when an
/// extension wants to ensure its middleware runs ahead of everything the
/// app registered itself (e.g. an APM probe that must wrap the entire
/// chain to time it correctly).
pub fn prepend_global_middleware<M: Middleware + 'static>(middleware: M) {
    let registry = GLOBAL_MIDDLEWARE.get_or_init(|| RwLock::new(Vec::new()));
    let mut vec = match registry.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let tid = TypeId::of::<M>();
    if vec.iter().any(|(existing, _)| *existing == tid) {
        tracing::debug!(
            "global middleware of this type is already registered; skipping the \
             prepend. Wrap it in a distinct newtype to register multiple instances."
        );
        return;
    }
    vec.insert(0, (tid, into_boxed(middleware)));
}

/// Whether a global middleware of this concrete type has been
/// registered. Laravel's `Kernel::hasMiddleware`.
pub fn has_global_middleware<M: Middleware + 'static>() -> bool {
    let tid = TypeId::of::<M>();
    let Some(lock) = GLOBAL_MIDDLEWARE.get() else {
        return false;
    };
    let guard = match lock.read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.iter().any(|(existing, _)| *existing == tid)
}

/// Number of global middleware currently registered. Useful for
/// boot-time diagnostics and assertions in test suites that snapshot the
/// registry around their setup.
pub fn global_middleware_count() -> usize {
    let Some(lock) = GLOBAL_MIDDLEWARE.get() else {
        return 0;
    };
    let guard = match lock.read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.len()
}

/// Clear every global middleware. Test-only convenience for suites that
/// need the snapshot to start empty (e.g. asserting boot-time
/// registration without inheriting whatever the rest of the suite
/// registered).
#[doc(hidden)]
pub fn clear_global_middleware_for_test() {
    let Some(lock) = GLOBAL_MIDDLEWARE.get() else {
        return;
    };
    let mut guard = match lock.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.clear();
}

/// Registry for global middleware that runs on every request
///
/// # Example
///
/// ```rust,no_run
/// # use suprnova::Server;
/// # use suprnova::routing::Router;
/// # use suprnova::middleware::{Middleware, Next};
/// # use suprnova::http::{Request, Response};
/// # use suprnova::async_trait;
/// # #[derive(Clone)] struct LoggingMiddleware;
/// # #[async_trait]
/// # impl Middleware for LoggingMiddleware {
/// #     async fn handle(&self, req: Request, next: Next) -> Response { next(req).await }
/// # }
/// # #[derive(Clone)] struct CorsMiddleware;
/// # #[async_trait]
/// # impl Middleware for CorsMiddleware {
/// #     async fn handle(&self, req: Request, next: Next) -> Response { next(req).await }
/// # }
/// # async fn ex() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
/// # let router = Router::new();
/// Server::from_config(router)?
///     .middleware(LoggingMiddleware)  // Global middleware
///     .middleware(CorsMiddleware)
///     .run()
///     .await?;
/// # Ok(()) }
/// ```
pub struct MiddlewareRegistry {
    /// Middleware that runs on every request (in order)
    global: Vec<BoxedMiddleware>,
}

impl MiddlewareRegistry {
    /// Create a new empty middleware registry
    pub fn new() -> Self {
        Self { global: Vec::new() }
    }

    /// Create a registry pre-populated with globally registered middleware.
    ///
    /// This pulls middleware registered via `global_middleware!` in
    /// bootstrap.rs. The list is **snapshotted at call time**: register
    /// every global middleware BEFORE constructing the `Server`. The
    /// scaffolded boot order does this for you (`bootstrap()` runs, then
    /// `Server::from_config` / `Server::new` builds the server). A
    /// `global_middleware!` call made AFTER a server is built does not
    /// retroactively apply to that already-constructed server — that is
    /// deliberate, so a running server's middleware stack cannot shift
    /// underneath it.
    pub fn from_global() -> Self {
        Self {
            global: get_global_middleware(),
        }
    }

    /// Append global middleware that runs on every request
    ///
    /// Global middleware runs in the order they are added, before any
    /// route-specific middleware.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use suprnova::middleware::{Middleware, MiddlewareRegistry, Next};
    /// # use suprnova::http::{Request, Response};
    /// # use suprnova::async_trait;
    /// # #[derive(Clone)] struct LoggingMiddleware;
    /// # #[async_trait]
    /// # impl Middleware for LoggingMiddleware {
    /// #     async fn handle(&self, req: Request, next: Next) -> Response { next(req).await }
    /// # }
    /// # #[derive(Clone)] struct CorsMiddleware;
    /// # #[async_trait]
    /// # impl Middleware for CorsMiddleware {
    /// #     async fn handle(&self, req: Request, next: Next) -> Response { next(req).await }
    /// # }
    /// # let m = MiddlewareRegistry::new();
    /// m.append(LoggingMiddleware)
    ///  .append(CorsMiddleware)
    /// # ;
    /// ```
    pub fn append<M: Middleware + 'static>(mut self, middleware: M) -> Self {
        self.global.push(into_boxed(middleware));
        self
    }

    /// Insert global middleware at the front of the chain, before any
    /// existing entry. Mirrors Laravel's `Kernel::prependMiddleware`.
    ///
    /// Use when an embedder needs to wrap the entire chain (e.g. a
    /// tracing probe that must outerly time the rest of the stack).
    pub fn prepend<M: Middleware + 'static>(mut self, middleware: M) -> Self {
        self.global.insert(0, into_boxed(middleware));
        self
    }

    /// Push a pre-boxed global middleware. Companion to [`Self::append`]
    /// for callers that already hold a `BoxedMiddleware` (e.g. from a
    /// registry snapshot or alias resolution).
    pub fn append_boxed(mut self, middleware: BoxedMiddleware) -> Self {
        self.global.push(middleware);
        self
    }

    /// Insert a pre-boxed global middleware at the front of the chain.
    pub fn prepend_boxed(mut self, middleware: BoxedMiddleware) -> Self {
        self.global.insert(0, middleware);
        self
    }

    /// Get the list of global middleware.
    ///
    /// The returned slice is immutable after [`Self::from_global`] /
    /// [`Self::append`] snapshotted it at server construction time, so
    /// the request hot path can clone its entries cheaply: each
    /// [`BoxedMiddleware`] is an `Arc`, so `iter().cloned()` is just a
    /// refcount bump, not a deep copy. Callers that build a per-request
    /// [`crate::middleware::MiddlewareChain`] should pair this with
    /// [`crate::middleware::MiddlewareChain::with_capacity`] to also
    /// avoid re-allocating the backing `Vec`.
    pub fn global_middleware(&self) -> &[BoxedMiddleware] {
        &self.global
    }

    /// Number of global middleware. Convenience for tests and
    /// diagnostics so callers don't have to write `.global_middleware().len()`.
    pub fn len(&self) -> usize {
        self.global.len()
    }

    /// Whether the registry has no global middleware.
    pub fn is_empty(&self) -> bool {
        self.global.is_empty()
    }
}

impl Default for MiddlewareRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{Request, Response};
    use async_trait::async_trait;

    struct ProbeA;
    #[async_trait]
    impl Middleware for ProbeA {
        async fn handle(&self, request: Request, next: super::super::Next) -> Response {
            next(request).await
        }
    }

    struct ProbeB;
    #[async_trait]
    impl Middleware for ProbeB {
        async fn handle(&self, request: Request, next: super::super::Next) -> Response {
            next(request).await
        }
    }

    /// Registration is once-per-type. Operates on a LOCAL vector so the
    /// assertion is independent of the process-global `GLOBAL_MIDDLEWARE`
    /// (and of any other test that may touch it concurrently).
    #[test]
    fn insert_unique_global_skips_duplicate_types() {
        let mut reg: Vec<(TypeId, BoxedMiddleware)> = Vec::new();

        assert!(
            insert_unique_global(&mut reg, ProbeA),
            "the first ProbeA registration inserts"
        );
        assert_eq!(reg.len(), 1);

        assert!(
            !insert_unique_global(&mut reg, ProbeA),
            "a second registration of the same type is skipped"
        );
        assert_eq!(reg.len(), 1, "a duplicate type must not grow the registry");

        assert!(
            insert_unique_global(&mut reg, ProbeB),
            "a different middleware type still registers"
        );
        assert_eq!(reg.len(), 2);
    }
}
