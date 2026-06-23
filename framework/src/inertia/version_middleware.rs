//! Asset-version mismatch middleware.
//!
//! Per the Inertia v3 protocol (see `core-concepts/the-protocol.mdx`),
//! Inertia GET requests carry an `X-Inertia-Version` header. The server
//! compares that to its configured version; on mismatch the server returns
//! `409 Conflict` with an `X-Inertia-Location` header pointing at the
//! current URL. The client then performs a full-page visit to pick up the
//! new assets.
//!
//! Non-GET requests are exempt — the spec says version mismatch on
//! POST/PUT/PATCH/DELETE resolves naturally on the redirect that follows
//! the request (which IS a GET, and that GET will trigger the 409).
//!
//! ## Wiring
//!
//! This middleware is **opt-in**. Register globally from your app's
//! bootstrap so it runs on every request:
//!
//! ```rust,no_run
//! use suprnova::{global_middleware, InertiaConfig, InertiaVersionMiddleware};
//!
//! pub fn register() {
//!     let version = env!("CARGO_PKG_VERSION");
//!     let cfg = InertiaConfig::new().version(version);
//!     let _ = cfg;
//!     global_middleware!(InertiaVersionMiddleware::new(version));
//! }
//! ```
//!
//! Without this middleware, asset-version mismatch is silent — clients
//! continue to use the cached SPA bundle against a server emitting a
//! newer version.

use crate::http::{HttpResponse, Request, Response};
use crate::inertia::config::VersionResolver;
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;

/// Asset-version mismatch detector. Compares the request's
/// `X-Inertia-Version` against the configured version and returns
/// `409 + X-Inertia-Location: <url>` on mismatch.
///
/// Accepts either a static version string or a dynamic resolver via
/// [`VersionResolver`]. The dynamic resolver runs on every request so
/// the middleware stays in sync with build-time changes (hot reloads,
/// rolling deploys).
pub struct InertiaVersionMiddleware {
    version: VersionResolver,
}

impl InertiaVersionMiddleware {
    /// Create a new middleware with a static asset version. Use
    /// [`with_resolver`](Self::with_resolver) for dynamic versions.
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: VersionResolver::Static(version.into()),
        }
    }

    /// Create a new middleware that resolves the asset version via the
    /// given closure on every request. Wrap any caching inside the
    /// closure.
    pub fn with_resolver<F>(f: F) -> Self
    where
        F: Fn() -> String + Send + Sync + 'static,
    {
        Self {
            version: VersionResolver::with(f),
        }
    }
}

#[async_trait]
impl Middleware for InertiaVersionMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Only act on Inertia XHR requests. Standard browser visits (no
        // X-Inertia header) reload the full HTML anyway.
        if !request.is_inertia() {
            return next(request).await;
        }

        // Per the protocol, only GETs return 409 for version mismatch.
        // Other methods (POST/PUT/PATCH/DELETE) flow through; their
        // redirect-after responses will trigger the 409 on the GET that
        // follows them.
        if request.method() != hyper::Method::GET {
            return next(request).await;
        }

        let server_version = self.version.resolve();
        let client_version = request.inertia_version().unwrap_or("");
        if client_version == server_version {
            return next(request).await;
        }

        // Mismatch — bounce the client to do a full-page visit at the
        // same URL so it picks up the new assets. Preserve the query
        // string: a 409 on `/search?q=rust` must redirect back to the
        // same search, not bare `/search` (which would silently drop
        // pagination cursors, filter state, and form-submitted GET
        // params on every asset-version mismatch).
        let url = request
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str().to_string())
            .unwrap_or_else(|| request.path().to_string());
        Err(HttpResponse::new()
            .status(409)
            .header("X-Inertia-Location", url))
    }
}
