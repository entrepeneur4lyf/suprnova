//! [`Limit`] and friends — value type for "N attempts per window, optionally
//! keyed and with a response/after callback".
//!
//! Mirrors `Illuminate\Cache\RateLimiting\Limit`:
//!
//! - `Limit::per_second / per_minute / per_minutes / per_hour / per_day` —
//!   classic shorthand constructors.
//! - `Limit::none()` returns an [`Unlimited`] (a limit that never trips).
//! - `.by(key)` — set the bucket key (typically `request.user().id()` or
//!   `request.ip()`).
//! - `.response(callback)` — generate a custom response when the limit is
//!   exceeded.
//! - `.after(callback)` — only count the attempt if `callback(response)` is
//!   true (Laravel's "only fail-on-failure" pattern).
//!
//! A [`GlobalLimit`] is just a [`Limit`] whose key is empty: it applies
//! globally instead of per-bucket. [`Unlimited`] is a [`GlobalLimit`] with
//! `max_attempts = i64::MAX` and is the type [`Limit::none`] returns.
//!
//! The named-limiter callback registered via
//! [`RateLimiter::define`](super::RateLimiter::define) can return any
//! [`LimitResult`] — a single [`Limit`], a list of [`Limit`]s (each limit is
//! applied), or a fully-formed [`crate::Response`] to short-circuit. This
//! matches Laravel's three-shape return contract for `RateLimiter::for(...)`.

use crate::Request;
use crate::http::HttpResponse;
use std::sync::Arc;
use std::time::Duration;

/// Closure type for [`Limit::after`] — the post-response predicate that
/// decides whether to debit the limit. `true` means "burn the attempt".
pub type AfterCallback = Arc<dyn Fn(&HttpResponse) -> bool + Send + Sync>;

/// Closure type for [`Limit::response`] — the custom 429 builder. Receives
/// the failing request so it can render context-aware errors.
pub type ResponseCallback = Arc<dyn Fn(&Request) -> HttpResponse + Send + Sync>;

/// The shape `RateLimiter::define` (Laravel's `for(name, callback)`)
/// callbacks may return.
///
/// `Single(Limit)` and `Many(Vec<Limit>)` apply one or more limits per
/// request; `Response(Response)` short-circuits the request with the
/// supplied response (used by Laravel's `RateLimiter::for('login', fn (req)
/// => $req->user()->is_admin() ? Limit::none() : Limit::per_minute(5))`
/// pattern when an admin gets unlimited access).
pub enum LimitResult {
    /// Apply a single limit.
    Single(Limit),
    /// Apply every supplied limit; the first one to trip wins.
    Many(Vec<Limit>),
    /// Short-circuit with the supplied response — used to give a caller
    /// unlimited access OR to refuse the request outright.
    Response(HttpResponse),
}

impl From<Limit> for LimitResult {
    fn from(l: Limit) -> Self {
        LimitResult::Single(l)
    }
}

impl From<Vec<Limit>> for LimitResult {
    fn from(v: Vec<Limit>) -> Self {
        LimitResult::Many(v)
    }
}

impl From<HttpResponse> for LimitResult {
    fn from(r: HttpResponse) -> Self {
        LimitResult::Response(r)
    }
}

/// A single rate-limit clause: `max_attempts` requests per `decay`
/// seconds, optionally keyed by `key` and gated by `after`/`response`
/// callbacks.
///
/// Construct via the per-* shorthands, then chain `.by(...)`,
/// `.response(...)`, `.after(...)`. The value is `Clone` so the same
/// limit can be applied to multiple requests through the named-limiter
/// callback.
#[derive(Clone)]
pub struct Limit {
    /// The bucket key. Empty string means "global" (every caller hits the
    /// same bucket). [`Limit::by`] is the canonical way to set this.
    pub key: String,
    /// The maximum number of attempts allowed within `decay`.
    pub max_attempts: i64,
    /// The window during which `max_attempts` is measured. Laravel calls
    /// this `decay_seconds`; we keep `Duration` to be true to Rust idiom,
    /// and expose `decay_seconds()` for the literal Laravel field.
    pub decay: Duration,
    /// If set, after the wrapped request runs, the limiter will only
    /// count the hit when `after(response)` returns true. Mirrors
    /// Laravel's `Limit::after($callback)` — the canonical use is "only
    /// count failed login attempts": `after(|r| r.status() >= 400)`.
    pub after_callback: Option<AfterCallback>,
    /// If set, takes precedence over the default 429 when the limit is
    /// exceeded. Mirrors `Limit::response($callback)`. The callback receives
    /// the failing request so it can render context-aware errors (e.g. an
    /// HTML page vs a JSON envelope).
    pub response_callback: Option<ResponseCallback>,
}

impl Limit {
    /// Bare constructor — `max_attempts` per `decay`, no key, no callbacks.
    pub fn new(max_attempts: i64, decay: Duration) -> Self {
        Self {
            key: String::new(),
            max_attempts,
            decay,
            after_callback: None,
            response_callback: None,
        }
    }

    /// `max_attempts` per `decay_seconds` second(s). Default 1-second window
    /// matches Laravel's `Limit::perSecond($max)` shorthand.
    pub fn per_second(max_attempts: i64, decay_seconds: u64) -> Self {
        Self::new(max_attempts, Duration::from_secs(decay_seconds))
    }

    /// `max_attempts` per minute (or per `decay_minutes` minutes when set).
    pub fn per_minute(max_attempts: i64) -> Self {
        Self::new(max_attempts, Duration::from_secs(60))
    }

    /// `max_attempts` per `decay_minutes` minute(s). Laravel ships this as
    /// `Limit::perMinutes($decayMinutes, $maxAttempts)` — argument order
    /// flipped — we match the Laravel signature to keep migration mechanical.
    pub fn per_minutes(decay_minutes: u64, max_attempts: i64) -> Self {
        Self::new(max_attempts, Duration::from_secs(60 * decay_minutes))
    }

    /// `max_attempts` per hour (or per `decay_hours` hours when set).
    pub fn per_hour(max_attempts: i64) -> Self {
        Self::new(max_attempts, Duration::from_secs(60 * 60))
    }

    /// `max_attempts` per `decay_hours` hour(s).
    pub fn per_hours(decay_hours: u64, max_attempts: i64) -> Self {
        Self::new(max_attempts, Duration::from_secs(60 * 60 * decay_hours))
    }

    /// `max_attempts` per day (24h).
    pub fn per_day(max_attempts: i64) -> Self {
        Self::new(max_attempts, Duration::from_secs(60 * 60 * 24))
    }

    /// `max_attempts` per `decay_days` day(s).
    pub fn per_days(decay_days: u64, max_attempts: i64) -> Self {
        Self::new(max_attempts, Duration::from_secs(60 * 60 * 24 * decay_days))
    }

    /// A limit that never trips. Mirrors `Limit::none()` — returns an
    /// [`Unlimited`]. Use it from a named limiter to give a caller
    /// unlimited access (e.g. admins).
    pub fn none() -> Unlimited {
        Unlimited::new()
    }

    /// Set the bucket key. Idiomatic shape:
    ///
    /// ```rust,ignore
    /// Limit::per_minute(10).by(format!("user:{}", user.id))
    /// ```
    pub fn by(mut self, key: impl Into<String>) -> Self {
        self.key = key.into();
        self
    }

    /// Only count the attempt when `callback(response)` returns true.
    /// Laravel ships this as `Limit::after($callback)`. The canonical use:
    /// only debit attempt against the limit on a failed login.
    pub fn after<F>(mut self, callback: F) -> Self
    where
        F: Fn(&HttpResponse) -> bool + Send + Sync + 'static,
    {
        self.after_callback = Some(Arc::new(callback));
        self
    }

    /// Generate a custom response when this limit is exceeded. Without it
    /// the middleware returns 429 with a plain "Too Many Attempts" body.
    pub fn response<F>(mut self, callback: F) -> Self
    where
        F: Fn(&Request) -> HttpResponse + Send + Sync + 'static,
    {
        self.response_callback = Some(Arc::new(callback));
        self
    }

    /// The `decay` duration as a whole number of seconds. Convenience for
    /// computing `X-RateLimit-Reset` and `Retry-After` headers, which are
    /// integer-seconds-since-epoch and integer-seconds respectively.
    pub fn decay_seconds(&self) -> u64 {
        self.decay.as_secs()
    }

    /// Compute a fallback key for the limit. Laravel uses this to
    /// disambiguate two limits that collide on the same bucket key inside
    /// a single named-limiter callback (e.g. two `per_minute` clauses for
    /// the same user).
    pub fn fallback_key(&self) -> String {
        if self.key.is_empty() {
            format!(
                "attempts:{}:decay:{}",
                self.max_attempts,
                self.decay_seconds()
            )
        } else {
            format!(
                "{}:attempts:{}:decay:{}",
                self.key,
                self.max_attempts,
                self.decay_seconds()
            )
        }
    }
}

/// A limit with no key — every caller hits the same bucket. Equivalent to
/// `Limit::new(max, decay)`; the dedicated type is here for parity with
/// `Illuminate\Cache\RateLimiting\GlobalLimit` and to make caller intent
/// explicit at the type level.
pub struct GlobalLimit(pub Limit);

impl GlobalLimit {
    pub fn new(max_attempts: i64, decay: Duration) -> Self {
        Self(Limit::new(max_attempts, decay))
    }

    pub fn per_minute(max_attempts: i64) -> Self {
        Self(Limit::per_minute(max_attempts))
    }

    pub fn per_hour(max_attempts: i64) -> Self {
        Self(Limit::per_hour(max_attempts))
    }
}

impl From<GlobalLimit> for Limit {
    fn from(g: GlobalLimit) -> Self {
        g.0
    }
}

impl From<GlobalLimit> for LimitResult {
    fn from(g: GlobalLimit) -> Self {
        LimitResult::Single(g.0)
    }
}

/// A limit that never trips. Mirrors
/// `Illuminate\Cache\RateLimiting\Unlimited` (a [`GlobalLimit`] with
/// `max_attempts = PHP_INT_MAX`). [`Limit::none`] is the canonical
/// constructor.
///
/// Returning `Unlimited` from a named-limiter callback is the Laravel
/// pattern for "this caller bypasses the limit". The
/// [`ThrottleRequestsMiddleware`](super::ThrottleRequestsMiddleware)
/// recognises it and skips the attempt count entirely.
pub struct Unlimited(pub Limit);

impl Unlimited {
    /// Create a never-trips limit. `max_attempts = i64::MAX`,
    /// `decay = 60s` (irrelevant since the limit never trips).
    pub fn new() -> Self {
        Self(Limit::new(i64::MAX, Duration::from_secs(60)))
    }
}

impl Default for Unlimited {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Unlimited> for Limit {
    fn from(u: Unlimited) -> Self {
        u.0
    }
}

impl From<Unlimited> for LimitResult {
    fn from(u: Unlimited) -> Self {
        LimitResult::Single(u.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpResponse;

    #[test]
    fn per_minute_window_is_60_seconds() {
        let l = Limit::per_minute(5);
        assert_eq!(l.decay_seconds(), 60);
        assert_eq!(l.max_attempts, 5);
        assert!(l.key.is_empty());
    }

    #[test]
    fn per_minutes_swaps_args_to_match_laravel_signature() {
        // Laravel: perMinutes($decayMinutes, $maxAttempts) — minutes first.
        let l = Limit::per_minutes(3, 100);
        assert_eq!(l.decay_seconds(), 180);
        assert_eq!(l.max_attempts, 100);
    }

    #[test]
    fn per_day_is_one_full_day() {
        let l = Limit::per_day(1000);
        assert_eq!(l.decay_seconds(), 86_400);
    }

    #[test]
    fn by_sets_the_bucket_key() {
        let l = Limit::per_minute(5).by("user:42");
        assert_eq!(l.key, "user:42");
    }

    #[test]
    fn fallback_key_disambiguates_per_max_and_decay() {
        let a = Limit::per_minute(10).by("user:1");
        let b = Limit::per_minute(20).by("user:1");
        assert_ne!(a.fallback_key(), b.fallback_key());
    }

    #[test]
    fn fallback_key_skips_prefix_when_global() {
        let g = Limit::per_minute(10);
        assert_eq!(g.fallback_key(), "attempts:10:decay:60");
    }

    #[test]
    fn unlimited_is_never_tripped() {
        let u = Limit::none();
        assert_eq!(u.0.max_attempts, i64::MAX);
    }

    #[test]
    fn response_callback_is_stored() {
        let l = Limit::per_minute(5).response(|_req| HttpResponse::text("custom").status(418));
        assert!(l.response_callback.is_some());
    }

    #[test]
    fn after_callback_is_stored() {
        let l = Limit::per_minute(5).after(|r| r.status_code() >= 400);
        assert!(l.after_callback.is_some());
    }

    #[test]
    fn global_limit_into_limit_unwraps_inner() {
        let g = GlobalLimit::per_minute(100);
        let inner: Limit = g.into();
        assert_eq!(inner.max_attempts, 100);
        assert!(inner.key.is_empty());
    }

    #[test]
    fn limit_result_from_single_limit() {
        let r: LimitResult = Limit::per_minute(5).into();
        match r {
            LimitResult::Single(l) => assert_eq!(l.max_attempts, 5),
            _ => panic!("expected Single"),
        }
    }

    #[test]
    fn limit_result_from_many_limits() {
        let r: LimitResult = vec![Limit::per_minute(5), Limit::per_hour(100)].into();
        match r {
            LimitResult::Many(v) => assert_eq!(v.len(), 2),
            _ => panic!("expected Many"),
        }
    }
}
