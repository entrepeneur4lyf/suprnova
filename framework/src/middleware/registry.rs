//! Middleware registry for global middleware
//!
//! Configure global middleware in `bootstrap.rs` using the `global_middleware!` macro,
//! or use `Server::middleware()` for manual configuration.

use super::{BoxedMiddleware, Middleware, into_boxed};
use std::sync::{OnceLock, RwLock};

/// Global middleware registry (populated via `global_middleware!` macro in bootstrap.rs)
static GLOBAL_MIDDLEWARE: OnceLock<RwLock<Vec<BoxedMiddleware>>> = OnceLock::new();

/// Register a global middleware that runs on every request.
///
/// Called by the `global_middleware!` macro. Middleware runs in
/// registration order.
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
/// ```rust,ignore
/// // In bootstrap.rs
/// global_middleware!(LoggingMiddleware);
/// global_middleware!(CorsMiddleware);
/// ```
pub fn register_global_middleware<M: Middleware + 'static>(middleware: M) {
    let registry = GLOBAL_MIDDLEWARE.get_or_init(|| RwLock::new(Vec::new()));
    let mut vec = match registry.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    vec.push(into_boxed(middleware));
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
    guard.clone()
}

/// Registry for global middleware that runs on every request
///
/// # Example
///
/// ```rust,ignore
/// Server::from_config(router)
///     .middleware(LoggingMiddleware)  // Global middleware
///     .middleware(CorsMiddleware)
///     .run()
///     .await;
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

    /// Create a registry pre-populated with globally registered middleware
    ///
    /// This pulls middleware registered via `global_middleware!` in bootstrap.rs.
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
    /// ```rust,ignore
    /// m.append(LoggingMiddleware)
    ///  .append(CorsMiddleware)
    /// ```
    pub fn append<M: Middleware + 'static>(mut self, middleware: M) -> Self {
        self.global.push(into_boxed(middleware));
        self
    }

    /// Get the list of global middleware
    pub fn global_middleware(&self) -> &[BoxedMiddleware] {
        &self.global
    }
}

impl Default for MiddlewareRegistry {
    fn default() -> Self {
        Self::new()
    }
}
