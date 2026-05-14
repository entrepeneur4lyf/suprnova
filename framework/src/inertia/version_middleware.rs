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
//! ```rust,ignore
//! use suprnova::{global_middleware, InertiaConfig, InertiaVersionMiddleware};
//!
//! pub fn register() {
//!     let cfg = InertiaConfig::new().version(env!("CARGO_PKG_VERSION"));
//!     global_middleware!(InertiaVersionMiddleware::new(cfg.version));
//! }
//! ```
//!
//! Without this middleware, asset-version mismatch is silent — clients
//! continue to use the cached SPA bundle against a server emitting a
//! newer version.

use crate::http::{HttpResponse, Request, Response};
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;

/// Asset-version mismatch detector. Compares the request's
/// `X-Inertia-Version` against a configured version string and returns
/// `409 + X-Inertia-Location: <url>` on mismatch.
pub struct InertiaVersionMiddleware {
    version: String,
}

impl InertiaVersionMiddleware {
    /// Create a new middleware with the configured asset version. Typically
    /// constructed from [`crate::inertia::InertiaConfig::version`].
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
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

        let client_version = request.inertia_version().unwrap_or("");
        if client_version == self.version {
            return next(request).await;
        }

        // Mismatch — bounce the client to do a full-page visit at the
        // same URL so it picks up the new assets.
        let url = request.path().to_string();
        Err(HttpResponse::text("")
            .status(409)
            .header("X-Inertia-Location", url))
    }
}
