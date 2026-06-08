//! 302 → 303 auto-conversion middleware.
//!
//! Per the Inertia v3 protocol:
//!
//! > When redirecting after a `PUT`, `PATCH`, or `DELETE` request, you
//! > must use a `303` response code, otherwise the subsequent request
//! > will not be treated as a `GET` request. A `303` redirect is very
//! > similar to a `302` redirect; however, the follow-up request is
//! > explicitly changed to a `GET` request.
//!
//! Without this conversion, browsers may re-submit the original method
//! to the redirect target — breaking form-create-then-redirect flows.
//! Laravel's Inertia adapter ships this conversion inside its own
//! middleware; Suprnova ships it as an opt-in via `global_middleware!`.

use crate::http::{Request, Response};
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;

/// Middleware that rewrites `302 Found` redirects to `303 See Other` for
/// Inertia-initiated requests so the client follows them with `GET`.
pub struct Inertia303Middleware;

impl Inertia303Middleware {
    /// Build a new `Inertia303Middleware`. Stateless — no arguments needed.
    pub fn new() -> Self {
        Self
    }
}

impl Default for Inertia303Middleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for Inertia303Middleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Capture method + inertia-ness before passing the request along
        // (next() consumes it).
        let is_inertia = request.is_inertia();
        let method = request.method().clone();
        let response = next(request).await;

        // Only act when an Inertia non-GET produced a 302 redirect.
        if !is_inertia || method == hyper::Method::GET {
            return response;
        }

        // Unwrap whichever Result side the response came back on so we
        // can inspect status, then re-wrap the same way.
        let was_ok = response.is_ok();
        let mut http = response.unwrap_or_else(|e| e);
        if http.status_code() == 302 {
            http = http.status(303);
        }
        if was_ok { Ok(http) } else { Err(http) }
    }
}
