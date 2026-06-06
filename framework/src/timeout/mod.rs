//! Per-request timeout middleware.
//!
//! Bounds how long a request handler may take to **produce a response**.
//! A slow handler or a hung database query can otherwise hold a connection
//! open indefinitely; [`TimeoutMiddleware`] gives the request pipeline a
//! hard deadline and returns `503 Service Unavailable` when it is exceeded.
//!
//! # What is — and is not — bounded
//!
//! The deadline races [`next(request)`](crate::middleware::Next), which
//! resolves the moment the handler **returns its [`HttpResponse`]**. It does
//! NOT bound how long the response *body* takes to stream afterwards. That
//! distinction is load-bearing:
//!
//! - **Normal handlers** build their full body before returning, so the
//!   deadline effectively bounds total handler time. ✔ bounded.
//! - **Streaming responses** ([`HttpResponse::sse`](crate::http::HttpResponse::sse),
//!   [`stream_bytes`](crate::http::HttpResponse::stream_bytes)) return
//!   *immediately* with a lazy body that hyper drains after the middleware
//!   chain has already completed. The deadline never observes the stream's
//!   lifetime. ✔ naturally excluded — an SSE stream can stay open for hours
//!   under a 30-second timeout.
//! - **WebSocket upgrades** are skipped explicitly (see below) and, today,
//!   also take a separate server path that bypasses global middleware
//!   entirely. ✔ excluded.
//!
//! # WebSocket carve-out
//!
//! [`TimeoutMiddleware`] returns early — without arming the deadline — for
//! any request carrying `Upgrade: websocket`. Currently WS upgrades are
//! dispatched by [`server::handle_ws_upgrade`](crate::server) which never
//! runs global middleware, so this guard is **defense in depth** for the day
//! global middleware is also applied to upgrades.
//!
//! # Cancel safety
//!
//! When the deadline elapses the in-flight handler future is **dropped** at
//! its current await point. Anything held across that point is released by
//! its `Drop` impl — open database transactions roll back, `Mutex`/`RwLock`
//! guards release, file handles close. Work moved off the request via
//! [`tokio::spawn`] is detached and will **not** be cancelled, so keep
//! handlers cancel-safe: don't rely on code after a long `.await` running if
//! the request might time out.
//!
//! # Installation
//!
//! Install globally for a process-wide ceiling, or per-route/group to
//! tighten specific endpoints:
//!
//! ```rust,ignore
//! use std::time::Duration;
//! use suprnova::{global_middleware, Router, TimeoutMiddleware};
//!
//! // Global: every HTTP route gets the 30s default ceiling.
//! global_middleware!(TimeoutMiddleware::default());
//!
//! // Per-route: tighten a specific endpoint to 5 seconds.
//! Router::new()
//!     .get("/report", report_handler)
//!     .middleware(TimeoutMiddleware::seconds(5));
//! ```
//!
//! Global middleware runs **outside** route middleware, so a global timeout
//! is an outer ceiling and a per-route timeout can only *tighten* it (the
//! inner, shorter deadline fires first). To let one route run *longer* than
//! the global default, either raise the global value or scope the global
//! middleware to a route group that excludes that endpoint.

use std::time::Duration;

use async_trait::async_trait;
use hyper::HeaderMap;

use crate::Request;
use crate::http::{HttpResponse, Response};
use crate::middleware::{Middleware, Next};

/// Default per-request deadline used by [`TimeoutMiddleware::default`]: 30s.
///
/// Chosen to match the database connect timeout (`DB_CONNECT_TIMEOUT`,
/// default 30s) so a request blocked on a brand-new connection and a request
/// blocked in the handler share one ceiling.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Middleware that fails a request with `503 Service Unavailable` if its
/// handler does not produce a response within the configured deadline.
///
/// See the [module documentation](self) for what the deadline bounds, the
/// SSE/WebSocket exclusions, cancel-safety semantics, and how global vs
/// per-route installation interact.
pub struct TimeoutMiddleware {
    duration: Duration,
}

impl TimeoutMiddleware {
    /// Build a timeout middleware with an explicit deadline.
    pub fn new(duration: Duration) -> Self {
        Self { duration }
    }

    /// Build a timeout middleware with a deadline in whole seconds.
    ///
    /// Convenience for the common case; equivalent to
    /// `TimeoutMiddleware::new(Duration::from_secs(secs))`.
    pub fn seconds(secs: u64) -> Self {
        Self {
            duration: Duration::from_secs(secs),
        }
    }

    /// The configured deadline.
    pub fn duration(&self) -> Duration {
        self.duration
    }
}

impl Default for TimeoutMiddleware {
    /// A 30-second deadline ([`DEFAULT_TIMEOUT`]).
    fn default() -> Self {
        Self {
            duration: DEFAULT_TIMEOUT,
        }
    }
}

#[async_trait]
impl Middleware for TimeoutMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // WebSocket upgrades stream for the lifetime of the connection, so a
        // request deadline must never bound them. See the module docs: this
        // guard is defense in depth.
        if is_websocket_upgrade(request.headers()) {
            return next(request).await;
        }

        // Capture the path before `next` consumes the request, so the
        // timeout log can name the offending route.
        let path = request.path().to_string();

        match tokio::time::timeout(self.duration, next(request)).await {
            Ok(response) => response,
            Err(_elapsed) => {
                tracing::warn!(
                    route = %path,
                    timeout_ms = self.duration.as_millis() as u64,
                    "request exceeded its timeout; returning 503 Service Unavailable"
                );
                Err(HttpResponse::text("Service Unavailable: request timed out").status(503))
            }
        }
    }
}

/// Whether `headers` describe a WebSocket upgrade (`Upgrade: websocket`,
/// case-insensitive on the token value). Header *name* lookup is already
/// case-insensitive via [`hyper::HeaderMap`].
fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(hyper::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::{CONNECTION, HeaderValue, UPGRADE};

    #[test]
    fn default_deadline_is_thirty_seconds() {
        assert_eq!(TimeoutMiddleware::default().duration(), DEFAULT_TIMEOUT);
        assert_eq!(DEFAULT_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn constructors_set_the_deadline() {
        assert_eq!(
            TimeoutMiddleware::new(Duration::from_millis(250)).duration(),
            Duration::from_millis(250)
        );
        assert_eq!(
            TimeoutMiddleware::seconds(7).duration(),
            Duration::from_secs(7)
        );
    }

    #[test]
    fn detects_websocket_upgrade_case_insensitively() {
        let mut headers = HeaderMap::new();
        headers.insert(UPGRADE, HeaderValue::from_static("websocket"));
        assert!(is_websocket_upgrade(&headers));

        let mut mixed = HeaderMap::new();
        mixed.insert(UPGRADE, HeaderValue::from_static("WebSocket"));
        assert!(
            is_websocket_upgrade(&mixed),
            "the Upgrade token must match case-insensitively"
        );
    }

    #[test]
    fn ignores_missing_or_non_websocket_upgrade() {
        assert!(
            !is_websocket_upgrade(&HeaderMap::new()),
            "no Upgrade header means not a websocket"
        );

        let mut h2c = HeaderMap::new();
        h2c.insert(UPGRADE, HeaderValue::from_static("h2c"));
        // A non-websocket upgrade (e.g. HTTP/2 cleartext) is still bounded.
        assert!(!is_websocket_upgrade(&h2c));

        // `Connection: upgrade` alone, without `Upgrade: websocket`, is not
        // a websocket and must remain bounded.
        let mut conn_only = HeaderMap::new();
        conn_only.insert(CONNECTION, HeaderValue::from_static("upgrade"));
        assert!(!is_websocket_upgrade(&conn_only));
    }
}
