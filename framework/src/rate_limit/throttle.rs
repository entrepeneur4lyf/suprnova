//! [`ThrottleRequestsMiddleware`] — HTTP wrapper around the
//! Cache-backed [`RateLimiter`] facade. Mirrors
//! `Illuminate\Routing\Middleware\ThrottleRequests`.
//!
//! Construct one of three ways:
//!
//! - [`ThrottleRequestsMiddleware::by_name`] — resolve a named limiter
//!   registered via [`RateLimiter::define`]. The named callback receives
//!   the `&Request` and returns a [`LimitResult`] (single limit, list of
//!   limits, or a short-circuit response).
//! - [`ThrottleRequestsMiddleware::with`] — provide a max-attempts /
//!   decay-minutes / prefix tuple directly. The literal Laravel
//!   `throttle:60,1` shape.
//! - [`ThrottleRequestsMiddleware::with_limits`] — build the
//!   [`Limit`]s in Rust and pass them through. Useful when the limits
//!   are computed at boot time and don't need to be named.
//!
//! Every wrapped response carries the `X-RateLimit-Limit` and
//! `X-RateLimit-Remaining` headers; 429 responses additionally carry
//! `Retry-After` and `X-RateLimit-Reset`. This matches Laravel's
//! `ThrottleRequests::getHeaders($maxAttempts, $remainingAttempts,
//! $retryAfter, $response)` shape.

use async_trait::async_trait;

use crate::Middleware;
use crate::Next;
use crate::Request;
use crate::http::{HttpResponse, Response};

use super::laravel::{NamedLimiterFn, RateLimiter};
use super::limit::{Limit, LimitResult};

use std::sync::Arc;

/// HTTP throttling middleware backed by the Cache-shape
/// [`RateLimiter`].
///
/// This is the Laravel-shape companion to
/// [`RateLimitMiddleware`](super::RateLimitMiddleware) (which wraps the
/// sliding-window [`RateLimiterDriver`](super::RateLimiterDriver) SPI).
/// Use [`ThrottleRequestsMiddleware`] when you want named limiters,
/// `X-RateLimit-*` headers, or `Limit::response(...)` short-circuits;
/// use the driver middleware when you want exact sliding-window
/// semantics against a non-Cache backend.
pub struct ThrottleRequestsMiddleware {
    mode: Mode,
    prefix: String,
}

enum Mode {
    Named(String),
    Inline {
        max_attempts: i64,
        decay_seconds: u64,
    },
    Limits(Vec<Limit>),
}

impl ThrottleRequestsMiddleware {
    /// Build a throttle middleware that resolves the named limiter at
    /// request time. Mirrors `ThrottleRequests::using('api')` /
    /// `throttle:api`.
    ///
    /// The limiter must have been registered via
    /// [`RateLimiter::define`]; otherwise every request returns
    /// `503 Service Unavailable` with a body that names the missing
    /// limiter (matching the `MissingRateLimiterException` that Laravel
    /// throws — Suprnova surfaces it as an HTTP response rather than
    /// panicking the worker thread).
    pub fn by_name(name: impl Into<String>) -> Self {
        Self {
            mode: Mode::Named(name.into()),
            prefix: String::new(),
        }
    }

    /// Build a throttle middleware with literal Laravel-shape
    /// `max,decay,prefix` arguments. Mirrors
    /// `ThrottleRequests::with($maxAttempts, $decayMinutes, $prefix)`.
    pub fn with(max_attempts: i64, decay_minutes: u64, prefix: impl Into<String>) -> Self {
        Self {
            mode: Mode::Inline {
                max_attempts,
                decay_seconds: 60 * decay_minutes,
            },
            prefix: prefix.into(),
        }
    }

    /// Build a throttle middleware from a list of explicit
    /// [`Limit`]s. The first limit to trip wins. This is the most
    /// Rust-idiomatic constructor and doesn't require a named-limiter
    /// registration.
    pub fn with_limits(limits: Vec<Limit>) -> Self {
        Self {
            mode: Mode::Limits(limits),
            prefix: String::new(),
        }
    }

    /// Set a prefix that is prepended to every limit's key. Mirrors
    /// the third positional argument of Laravel's
    /// `ThrottleRequests::with`.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }
}

#[async_trait]
impl Middleware for ThrottleRequestsMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let limits = match resolve_limits(&self.mode, &request) {
            ResolvedLimits::Ok(limits) => limits,
            ResolvedLimits::ShortCircuit(resp) => return Ok(resp),
            ResolvedLimits::MissingLimiter(name) => {
                tracing::warn!(name = %name, "throttle middleware: named limiter not registered");
                return Err(HttpResponse::text(format!(
                    "Rate limiter [{name}] is not defined. Register one with \
                     RateLimiter::define(\"{name}\", |req| ...) at boot."
                ))
                .status(503));
            }
        };

        // Apply each limit's gate. If any one says "too many attempts",
        // build the 429 response (or its custom-response equivalent) and
        // short-circuit. Matches Laravel's first-trip-wins pass through
        // `handleRequest`.
        for limit in &limits {
            let key = match prefixed_key(limit, &self.mode, &self.prefix) {
                Some(k) => k,
                None => continue, // Unlimited limit -- never trips.
            };
            if RateLimiter::too_many_attempts(&key, limit.max_attempts).await? {
                return Err(build_too_many_attempts_response(&request, limit, &key).await?);
            }
        }

        // Pre-debit any limit without an `after_callback`. Limits WITH
        // an after-callback only burn an attempt when the post-response
        // predicate matches, so we defer those until after `next`.
        for limit in &limits {
            if limit.after_callback.is_some() {
                continue;
            }
            let key = match prefixed_key(limit, &self.mode, &self.prefix) {
                Some(k) => k,
                None => continue,
            };
            if limit.max_attempts == i64::MAX {
                continue;
            }
            RateLimiter::hit(&key, limit.decay_seconds()).await?;
        }

        let response = next(request).await;

        // Apply after-callback gated hits, and inject X-RateLimit
        // headers on the outgoing response.
        match response {
            Ok(mut r) => {
                for limit in &limits {
                    let key = match prefixed_key(limit, &self.mode, &self.prefix) {
                        Some(k) => k,
                        None => continue,
                    };
                    if let Some(after) = &limit.after_callback
                        && after(&r)
                        && limit.max_attempts != i64::MAX
                    {
                        RateLimiter::hit(&key, limit.decay_seconds()).await?;
                    }
                    let remaining = RateLimiter::remaining(&key, limit.max_attempts).await?;
                    r = inject_headers(r, limit.max_attempts, remaining, None);
                }
                Ok(r)
            }
            Err(mut r) => {
                for limit in &limits {
                    let key = match prefixed_key(limit, &self.mode, &self.prefix) {
                        Some(k) => k,
                        None => continue,
                    };
                    if let Some(after) = &limit.after_callback
                        && after(&r)
                        && limit.max_attempts != i64::MAX
                    {
                        RateLimiter::hit(&key, limit.decay_seconds()).await?;
                    }
                    let remaining = RateLimiter::remaining(&key, limit.max_attempts).await?;
                    r = inject_headers(r, limit.max_attempts, remaining, None);
                }
                Err(r)
            }
        }
    }
}

enum ResolvedLimits {
    Ok(Vec<Limit>),
    ShortCircuit(HttpResponse),
    MissingLimiter(String),
}

fn resolve_limits(mode: &Mode, request: &Request) -> ResolvedLimits {
    match mode {
        Mode::Named(name) => {
            let Some(callback) = RateLimiter::limiter(name) else {
                return ResolvedLimits::MissingLimiter(name.clone());
            };
            match invoke(&callback, request) {
                LimitResult::Single(l) => ResolvedLimits::Ok(vec![l]),
                LimitResult::Many(v) => ResolvedLimits::Ok(v),
                LimitResult::Response(r) => ResolvedLimits::ShortCircuit(r),
            }
        }
        Mode::Inline {
            max_attempts,
            decay_seconds,
        } => {
            // Fall back to a request-derived key when no per-request
            // closure was supplied. Mirrors Laravel's
            // `resolveRequestSignature` (user-or-IP) but path is added
            // so two throttled routes with the same scope don't share
            // a bucket inadvertently. The middleware's `prefix` is
            // prepended later inside `prefixed_key`; baking it in here
            // would land it twice.
            let key = default_request_key(request);
            ResolvedLimits::Ok(vec![
                Limit::new(
                    *max_attempts,
                    std::time::Duration::from_secs(*decay_seconds),
                )
                .by(key),
            ])
        }
        Mode::Limits(limits) => ResolvedLimits::Ok(limits.clone()),
    }
}

fn invoke(cb: &Arc<NamedLimiterFn>, request: &Request) -> LimitResult {
    cb(request)
}

fn prefixed_key(limit: &Limit, mode: &Mode, prefix: &str) -> Option<String> {
    if limit.max_attempts == i64::MAX {
        return None;
    }
    let base = if limit.key.is_empty() {
        limit.fallback_key()
    } else {
        limit.key.clone()
    };
    let mut key = String::new();
    if !prefix.is_empty() {
        key.push_str(prefix);
        key.push(':');
    }
    if let Mode::Named(name) = mode {
        key.push_str(name);
        key.push(':');
    }
    key.push_str(&base);
    Some(RateLimiter::clean_rate_limiter_key(&key))
}

fn default_request_key(request: &Request) -> String {
    // Use `request.ip()` so the resolution goes through the
    // trusted-proxy gating in `Request::ip`: `X-Forwarded-For` /
    // `X-Real-IP` are honoured only when the TCP peer is in the
    // configured allowlist, and otherwise the TCP peer wins. Falls
    // back to the literal `"unknown"` only when no peer was threaded
    // into the request — that path is reserved for in-process tests
    // and the WS upgrade replay; production traffic always has a
    // peer.
    //
    // The middleware's `prefix` is prepended later inside
    // `prefixed_key`; do not bake it in here or it lands twice.
    //
    // # Security note
    //
    // Without a `TrustedProxiesConfig` opt-in, the bucket key is
    // grounded in the TCP peer — there is no XFF spoofing path. With
    // an opt-in, the operator has already attested that the listed
    // proxy hops can be trusted. Either way, the historical "every
    // anonymous caller shares one `anon` bucket" failure mode is
    // gone.
    let ip = request.ip().unwrap_or_else(|| "unknown".into());
    format!("ip:{ip}:path:{}", request.path())
}

async fn build_too_many_attempts_response(
    request: &Request,
    limit: &Limit,
    key: &str,
) -> Result<HttpResponse, HttpResponse> {
    let retry_after = RateLimiter::available_in(key)
        .await
        .map_err(|e| HttpResponse::text(format!("rate limiter error: {e}")).status(500))?;
    let remaining = 0_i64;
    if let Some(cb) = &limit.response_callback {
        let resp = cb(request);
        let resp = inject_headers(resp, limit.max_attempts, remaining, Some(retry_after));
        return Ok(resp);
    }
    let resp = HttpResponse::text("Too Many Attempts.").status(429);
    Ok(inject_headers(
        resp,
        limit.max_attempts,
        remaining,
        Some(retry_after),
    ))
}

fn inject_headers(
    response: HttpResponse,
    max_attempts: i64,
    remaining: i64,
    retry_after_secs: Option<u64>,
) -> HttpResponse {
    let mut resp = response
        .header("X-RateLimit-Limit", max_attempts.to_string())
        .header("X-RateLimit-Remaining", remaining.to_string());
    if let Some(retry) = retry_after_secs {
        resp = resp.header("Retry-After", retry.to_string());
        // Reset is the unix-seconds-since-epoch when the bucket reopens
        // (now + retry_after). Matches Laravel's `availableAt($retry)`.
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        resp = resp.header("X-RateLimit-Reset", (now + retry).to_string());
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixed_key_includes_named_limiter_name() {
        let mode = Mode::Named("api".into());
        let limit = Limit::per_minute(5).by("user:1");
        let key = prefixed_key(&limit, &mode, "").unwrap();
        assert_eq!(key, "api:user:1");
    }

    #[test]
    fn prefixed_key_returns_none_for_unlimited() {
        let mode = Mode::Inline {
            max_attempts: i64::MAX,
            decay_seconds: 60,
        };
        let limit: Limit = Limit::none().into();
        assert!(prefixed_key(&limit, &mode, "").is_none());
    }

    #[test]
    fn prefixed_key_uses_fallback_when_limit_unkeyed() {
        let mode = Mode::Inline {
            max_attempts: 10,
            decay_seconds: 60,
        };
        let limit = Limit::per_minute(10);
        let key = prefixed_key(&limit, &mode, "p").unwrap();
        // Prefix + fallback_key with no name.
        assert!(key.starts_with("p:"));
        assert!(key.contains("attempts:10:decay:60"));
    }

    #[test]
    fn prefixed_key_prepends_user_prefix() {
        let mode = Mode::Limits(vec![]);
        let limit = Limit::per_minute(5).by("user:1");
        let key = prefixed_key(&limit, &mode, "shop").unwrap();
        assert_eq!(key, "shop:user:1");
    }
}
