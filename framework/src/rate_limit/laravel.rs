//! Cache-backed [`RateLimiter`] facade — mirror of
//! `Illuminate\Cache\RateLimiter`.
//!
//! Implements Laravel's fixed-window counter on top of Suprnova's
//! [`crate::cache::Cache`] store. The window is anchored by a
//! `:timer` cache key that holds the available-at timestamp; the counter
//! itself accumulates hits under the bare `key`. When the timer is
//! missing or has expired, [`RateLimiter::too_many_attempts`] resets the
//! counter (this is the same recovery path Laravel uses).
//!
//! Use this facade when you want named limiters (`define`/`limiter`),
//! `attempt()` workflow callbacks, the `X-RateLimit-*` response-header
//! convention, or generally a Cache-backed counter API. For
//! one-slot-per-request enforcement against a non-cache backing store,
//! reach for [`RateLimitMiddleware`](super::RateLimitMiddleware) and the
//! [`RateLimiterDriver`](super::RateLimiterDriver) SPI instead.
//!
//! ## Storage layout
//!
//! For an attempt-counter key `K` and a decay of `D` seconds:
//!
//! - `K` — i64 counter incremented by every `hit`. Initial seed is 0
//!   (via `Cache::add`).
//! - `K:timer` — i64 unix-seconds-since-epoch when the window ends. Set
//!   via `Cache::add` so the deadline only seeds once per window (the
//!   counter resets on expiry, but the timer pinned the original
//!   deadline).
//!
//! Both keys carry the same TTL, so the cache store cleans them up
//! automatically when the window ends. This is the same two-key shape
//! Laravel uses in `Cache/RateLimiter.php:147-181`.

use crate::Request;
use crate::cache::Cache;
use crate::container::App;
use crate::error::FrameworkError;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use super::limit::LimitResult;

/// Registry of named rate limiters. Mirrors the `$limiters` array on
/// `Illuminate\Cache\RateLimiter`. The callback receives the [`Request`]
/// so it can derive a per-user / per-IP key, gate the limit on user
/// attributes (admins → unlimited), or return a fully-formed
/// [`HttpResponse`](crate::http::HttpResponse) (short-circuit).
///
/// The registry is process-global by design — Laravel apps register
/// limiters at boot (`AppServiceProvider::boot`) and never mutate them
/// at runtime. `RateLimiter::define("api", ...)` writes the callback
/// here; `RateLimiter::limiter("api")` looks it up.
pub struct NamedLimiterRegistry {
    inner: RwLock<HashMap<String, Arc<NamedLimiterFn>>>,
}

pub(crate) type NamedLimiterFn = dyn Fn(&Request) -> LimitResult + Send + Sync;

impl Default for NamedLimiterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl NamedLimiterRegistry {
    /// Construct an empty named-limiter registry.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    /// Insert a callback under `name`. Replaces any prior callback with
    /// the same name (matches Laravel).
    pub fn insert<F>(&self, name: impl Into<String>, callback: F)
    where
        F: Fn(&Request) -> LimitResult + Send + Sync + 'static,
    {
        let mut g = self.inner.write().expect("named limiter registry poisoned");
        g.insert(name.into(), Arc::new(callback));
    }

    /// Look up a callback by name. Returns `None` when no limiter under
    /// that name has been defined.
    pub fn get(&self, name: &str) -> Option<Arc<NamedLimiterFn>> {
        let g = self.inner.read().expect("named limiter registry poisoned");
        g.get(name).cloned()
    }

    /// Whether a limiter with this name exists.
    pub fn has(&self, name: &str) -> bool {
        let g = self.inner.read().expect("named limiter registry poisoned");
        g.contains_key(name)
    }
}

static REGISTRY: OnceLock<NamedLimiterRegistry> = OnceLock::new();

/// Access the process-wide named-limiter registry. Used by
/// [`ThrottleRequestsMiddleware`](super::ThrottleRequestsMiddleware) to
/// resolve named limiters by string at request time.
pub fn registry() -> &'static NamedLimiterRegistry {
    REGISTRY.get_or_init(NamedLimiterRegistry::new)
}

/// Laravel-shape rate-limiter facade. All methods are static — no
/// instance is constructed because the backing store is resolved from
/// the [`App`] container on every call.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::rate_limit::{Limit, RateLimiter};
///
/// // Register a named limiter at boot. Key by `req.ip()` — that goes
/// // through the trusted-proxy gating in `Request::ip`, so the bucket
/// // key reflects the real peer unless the operator has explicitly
/// // configured `APP_TRUSTED_PROXIES`.
/// RateLimiter::define("api", |req| {
///     let key = req.ip().unwrap_or_else(|| "anon".into());
///     Limit::per_minute(60).by(format!("ip:{key}")).into()
/// });
///
/// // Counter-style usage anywhere.
/// if RateLimiter::too_many_attempts("login:1.2.3.4", 5).await? {
///     return Err(HttpResponse::text("Too many attempts").status(429));
/// }
/// RateLimiter::hit("login:1.2.3.4", 60).await?;
/// ```
///
/// # Security note on bucket keys
///
/// Never key a limiter directly off `X-Forwarded-For` or `X-Real-IP`:
/// those headers are client-controlled and any inbound request can
/// carry them. Use [`Request::ip`](crate::Request::ip), which honours
/// the configured trusted-proxy allowlist — see
/// [`TrustedProxiesConfig`](crate::http::TrustedProxiesConfig). On a
/// deployment without a terminating proxy, an XFF-keyed limiter is a
/// DoS amplifier: an attacker rotates the header to mint unbounded
/// distinct keys. The in-memory driver's periodic sweep
/// ([`super::memory::InMemoryRateLimiter::with_periodic_sweep`]) is a
/// backstop against that growth, not a substitute for the right key.
pub struct RateLimiter;

const TIMER_SUFFIX: &str = ":timer";

fn unix_now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl RateLimiter {
    // ========================================================================
    // Named limiter registry
    // ========================================================================

    /// Register a named rate limiter. Mirrors `RateLimiter::for($name,
    /// $callback)`. The callback runs once per request and can return:
    ///
    /// - a [`Limit`](super::Limit) (`Limit::per_minute(5).by(req.ip())`),
    /// - a `Vec<Limit>` (apply every limit; first to trip wins), or
    /// - an [`HttpResponse`](crate::http::HttpResponse) (short-circuit; the request returns this
    ///   response immediately — used by Laravel's "admin gets
    ///   unlimited" pattern via `Limit::none()`, but the response form
    ///   also lets you refuse outright).
    ///
    /// `define` is the primary Rust-side name (`for` is a reserved
    /// keyword in Rust). The literal Laravel alias is exposed as
    /// `RateLimiter::r#for`.
    pub fn define<F>(name: impl Into<String>, callback: F)
    where
        F: Fn(&Request) -> LimitResult + Send + Sync + 'static,
    {
        registry().insert(name, callback);
    }

    /// Laravel-shape alias of [`RateLimiter::define`]. Lets migrated
    /// code keep the Laravel spelling under the rust raw-identifier
    /// escape (`RateLimiter::r#for("api", |req| ...)`).
    pub fn r#for<F>(name: impl Into<String>, callback: F)
    where
        F: Fn(&Request) -> LimitResult + Send + Sync + 'static,
    {
        Self::define(name, callback);
    }

    /// Resolve a named limiter callback. Returns `None` when no limiter
    /// has been registered under that name. Mirrors
    /// `RateLimiter::limiter($name)`.
    pub fn limiter(name: &str) -> Option<Arc<NamedLimiterFn>> {
        registry().get(name)
    }

    /// Whether a named limiter has been registered.
    pub fn has_limiter(name: &str) -> bool {
        registry().has(name)
    }

    // ========================================================================
    // Cache-backed fixed-window counter
    // ========================================================================

    /// The number of seconds until the bucket window ends. Returns 0
    /// when the window is not currently open (no hits yet, or the
    /// window has expired). Mirrors `RateLimiter::availableIn($key)`.
    pub async fn available_in(key: &str) -> Result<u64, FrameworkError> {
        let key = Self::clean_rate_limiter_key(key);
        let timer_key = format!("{key}{TIMER_SUFFIX}");
        let deadline: Option<i64> = Cache::get(&timer_key).await?;
        let Some(deadline) = deadline else {
            return Ok(0);
        };
        let now = unix_now_secs();
        Ok((deadline - now).max(0) as u64)
    }

    /// Current attempt count for the bucket. Mirrors
    /// `RateLimiter::attempts($key)`. Returns 0 when the key has never
    /// been hit (or has expired).
    pub async fn attempts(key: &str) -> Result<i64, FrameworkError> {
        let key = Self::clean_rate_limiter_key(key);
        let v: Option<i64> = Cache::get(&key).await?;
        Ok(v.unwrap_or(0))
    }

    /// Clear the counter (but leave the `:timer` deadline alone — the
    /// `clear` method below clears both). Mirrors
    /// `RateLimiter::resetAttempts($key)`.
    pub async fn reset_attempts(key: &str) -> Result<bool, FrameworkError> {
        let key = Self::clean_rate_limiter_key(key);
        Cache::forget(&key).await
    }

    /// Whether the bucket is over its limit. Mirrors
    /// `RateLimiter::tooManyAttempts($key, $maxAttempts)`.
    ///
    /// The implementation matches Laravel's contract exactly: if the
    /// counter has reached `max_attempts` AND the `:timer` is still
    /// present, the bucket is over its limit. If the counter has
    /// reached `max_attempts` but the `:timer` is gone (window
    /// expired), the counter is reset to zero and the bucket is NOT
    /// over its limit. This is what makes the window slide forward
    /// after a quota-exhausted period.
    pub async fn too_many_attempts(key: &str, max_attempts: i64) -> Result<bool, FrameworkError> {
        let n = Self::attempts(key).await?;
        if n < max_attempts {
            return Ok(false);
        }
        let timer_key = format!("{}{TIMER_SUFFIX}", Self::clean_rate_limiter_key(key));
        if Cache::has(&timer_key).await? {
            return Ok(true);
        }
        Self::reset_attempts(key).await?;
        Ok(false)
    }

    /// Increment the counter by 1 and seed the `:timer` deadline if
    /// missing. Mirrors `RateLimiter::hit($key, $decaySeconds)`.
    /// Returns the new counter value.
    pub async fn hit(key: &str, decay_seconds: u64) -> Result<i64, FrameworkError> {
        Self::increment(key, decay_seconds, 1).await
    }

    /// Increment the counter by `amount` and seed the `:timer` if
    /// missing. Mirrors `RateLimiter::increment($key, $decaySeconds,
    /// $amount)`. Returns the new counter value.
    ///
    /// The `:timer` deadline is set via `Cache::add` — only the first
    /// caller in the window pins the deadline; subsequent callers in
    /// the same window leave it untouched.
    pub async fn increment(
        key: &str,
        decay_seconds: u64,
        amount: i64,
    ) -> Result<i64, FrameworkError> {
        let key = Self::clean_rate_limiter_key(key);
        let timer_key = format!("{key}{TIMER_SUFFIX}");
        let decay = Duration::from_secs(decay_seconds);
        let available_at = unix_now_secs() + decay_seconds as i64;
        // Anchor the window deadline. `Cache::add` is no-op when the
        // key already exists, so a mid-window hit cannot shift the
        // window forward.
        Cache::add(&timer_key, &available_at, Some(decay)).await?;
        // Seed the counter at zero on first hit so `increment` has
        // something to bump. Mirrors Laravel's `add($key, 0, $decay)`
        // before the increment call.
        Cache::add(&key, &0_i64, Some(decay)).await?;
        let new_value = Cache::increment(&key, amount).await?;
        Ok(new_value)
    }

    /// Decrement the counter by `amount`. Mirrors
    /// `RateLimiter::decrement($key, $decaySeconds, $amount)`.
    pub async fn decrement(
        key: &str,
        decay_seconds: u64,
        amount: i64,
    ) -> Result<i64, FrameworkError> {
        Self::increment(key, decay_seconds, -amount).await
    }

    /// Number of retries remaining before the bucket trips. Mirrors
    /// `RateLimiter::remaining($key, $maxAttempts)`.
    pub async fn remaining(key: &str, max_attempts: i64) -> Result<i64, FrameworkError> {
        let n = Self::attempts(key).await?;
        Ok((max_attempts - n).max(0))
    }

    /// Alias of [`RateLimiter::remaining`]. Mirrors Laravel's
    /// `retriesLeft($key, $maxAttempts)` — both are part of the public
    /// `RateLimiter` API and migrated code may use either spelling.
    pub async fn retries_left(key: &str, max_attempts: i64) -> Result<i64, FrameworkError> {
        Self::remaining(key, max_attempts).await
    }

    /// Drop both the counter and the `:timer` deadline for the bucket.
    /// Mirrors `RateLimiter::clear($key)`. The next `hit` opens a fresh
    /// window.
    pub async fn clear(key: &str) -> Result<(), FrameworkError> {
        let key = Self::clean_rate_limiter_key(key);
        let timer_key = format!("{key}{TIMER_SUFFIX}");
        Cache::forget(&key).await?;
        Cache::forget(&timer_key).await?;
        Ok(())
    }

    /// Sanitise a rate-limiter key. Mirrors
    /// `RateLimiter::cleanRateLimiterKey($key)` for the strip stage:
    /// removes `&abc;` HTML-entity markers from the user-supplied input
    /// so the resulting cache key stays printable. The function is
    /// deterministic and idempotent inside Suprnova; consumers who need
    /// byte-identical hashing with a PHP service should run their
    /// `htmlentities` pre-step on the input themselves before calling
    /// this — Laravel's `htmlentities` step encodes non-ASCII bytes,
    /// which is unnecessary for Rust `String`s but which we deliberately
    /// don't replicate.
    pub fn clean_rate_limiter_key(key: &str) -> String {
        // Strip `&abc;` entity markers — same shape as Laravel's
        // `preg_replace('/&([a-z])[a-z]+;/i', '$1', htmlentities($key))`.
        // We skip Laravel's preceding `htmlentities` because the binary
        // range of a Rust `String` is already UTF-8-clean — Laravel's
        // `htmlentities` is there to encode binary garbage into a safe
        // ASCII subset before the strip, which is moot here.
        let mut out = String::with_capacity(key.len());
        let chars: Vec<char> = key.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '&' {
                // Look for `&letter+;` of length >= 2 letters.
                let mut j = i + 1;
                if j < chars.len() && chars[j].is_ascii_alphabetic() {
                    let first = chars[j];
                    j += 1;
                    while j < chars.len() && chars[j].is_ascii_alphabetic() {
                        j += 1;
                    }
                    if j < chars.len() && chars[j] == ';' && (j - i - 1) >= 2 {
                        out.push(first);
                        i = j + 1;
                        continue;
                    }
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    /// Run `callback` if the bucket is under `max_attempts`; otherwise
    /// return `Ok(None)`. On success, the counter is bumped by 1 and
    /// `callback`'s output is returned wrapped in `Some`. Mirrors
    /// `RateLimiter::attempt($key, $maxAttempts, $callback,
    /// $decaySeconds)`.
    ///
    /// Distinct from [`hit`](Self::hit) + manual gate: `attempt` only
    /// counts the hit when the callback actually runs, which is the
    /// shape login forms want (don't burn an attempt if we never
    /// reached the work).
    pub async fn attempt<T, F, Fut>(
        key: &str,
        max_attempts: i64,
        callback: F,
        decay_seconds: u64,
    ) -> Result<Option<T>, FrameworkError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, FrameworkError>>,
    {
        if Self::too_many_attempts(key, max_attempts).await? {
            return Ok(None);
        }
        let result = callback().await?;
        Self::hit(key, decay_seconds).await?;
        Ok(Some(result))
    }

    /// Whether the Cache binding required by the facade is wired. Mirrors
    /// `Cache::is_initialized` — useful when bootstrapping environment
    /// matters (named limiters can register at any time; counter calls
    /// need a Cache store).
    pub fn is_cache_initialized() -> bool {
        // Resolve the cache store binding — same predicate the facade
        // uses internally.
        let _ = App::resolve_make::<dyn crate::cache::CacheStore>();
        Cache::is_initialized()
    }
}

#[cfg(test)]
mod tests {
    use super::super::limit::Limit;
    use super::*;
    use crate::cache::{CacheStore, InMemoryCache};
    use crate::container::testing::TestContainer;

    fn fresh_test_container() -> impl Drop {
        let g = TestContainer::fake();
        TestContainer::bind::<dyn CacheStore>(Arc::new(InMemoryCache::new()));
        g
    }

    #[test]
    fn clean_strips_entity_markers() {
        assert_eq!(RateLimiter::clean_rate_limiter_key("abc"), "abc");
        // `&amp;` -> `a`.
        assert_eq!(RateLimiter::clean_rate_limiter_key("a&amp;b"), "aab");
        // `&copy;` -> `c`.
        assert_eq!(
            RateLimiter::clean_rate_limiter_key("c&copy;d"),
            "ccd",
            "named entity should reduce to first letter"
        );
        // Bare `&` left alone.
        assert_eq!(RateLimiter::clean_rate_limiter_key("foo&bar"), "foo&bar");
    }

    #[test]
    fn clean_preserves_unicode() {
        assert_eq!(
            RateLimiter::clean_rate_limiter_key("user:Ω:42"),
            "user:Ω:42"
        );
    }

    #[tokio::test]
    async fn hit_increments_counter_and_seeds_timer() {
        let _g = fresh_test_container();
        let n = RateLimiter::hit("login:1", 60).await.unwrap();
        assert_eq!(n, 1);
        let n = RateLimiter::hit("login:1", 60).await.unwrap();
        assert_eq!(n, 2);

        assert_eq!(RateLimiter::attempts("login:1").await.unwrap(), 2);
        let avail = RateLimiter::available_in("login:1").await.unwrap();
        assert!(avail > 0 && avail <= 60);
    }

    #[tokio::test]
    async fn too_many_attempts_returns_true_when_at_limit() {
        let _g = fresh_test_container();
        RateLimiter::hit("k", 60).await.unwrap();
        RateLimiter::hit("k", 60).await.unwrap();
        RateLimiter::hit("k", 60).await.unwrap();
        assert!(RateLimiter::too_many_attempts("k", 3).await.unwrap());
        assert!(!RateLimiter::too_many_attempts("k", 4).await.unwrap());
    }

    #[tokio::test]
    async fn remaining_returns_zero_when_over_limit() {
        let _g = fresh_test_container();
        RateLimiter::hit("r", 60).await.unwrap();
        RateLimiter::hit("r", 60).await.unwrap();
        assert_eq!(RateLimiter::remaining("r", 5).await.unwrap(), 3);
        assert_eq!(RateLimiter::retries_left("r", 5).await.unwrap(), 3);
        RateLimiter::hit("r", 60).await.unwrap();
        RateLimiter::hit("r", 60).await.unwrap();
        RateLimiter::hit("r", 60).await.unwrap();
        assert_eq!(RateLimiter::remaining("r", 5).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn clear_resets_both_counter_and_timer() {
        let _g = fresh_test_container();
        RateLimiter::hit("clr", 60).await.unwrap();
        RateLimiter::hit("clr", 60).await.unwrap();
        assert_eq!(RateLimiter::attempts("clr").await.unwrap(), 2);
        RateLimiter::clear("clr").await.unwrap();
        assert_eq!(RateLimiter::attempts("clr").await.unwrap(), 0);
        assert_eq!(RateLimiter::available_in("clr").await.unwrap(), 0);
    }

    #[tokio::test]
    async fn reset_attempts_clears_only_counter() {
        let _g = fresh_test_container();
        RateLimiter::hit("ra", 60).await.unwrap();
        RateLimiter::hit("ra", 60).await.unwrap();
        let avail_before = RateLimiter::available_in("ra").await.unwrap();
        RateLimiter::reset_attempts("ra").await.unwrap();
        assert_eq!(RateLimiter::attempts("ra").await.unwrap(), 0);
        // Timer is still present.
        let avail_after = RateLimiter::available_in("ra").await.unwrap();
        assert!(avail_after > 0);
        assert!(avail_after <= avail_before);
    }

    #[tokio::test]
    async fn attempt_runs_callback_under_limit_and_skips_over_limit() {
        let _g = fresh_test_container();
        let counter = Arc::new(std::sync::atomic::AtomicI32::new(0));
        for _ in 0..3 {
            let c = counter.clone();
            let result = RateLimiter::attempt(
                "a",
                3,
                || async move {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok::<_, FrameworkError>(())
                },
                60,
            )
            .await
            .unwrap();
            assert!(result.is_some());
        }
        // Fourth call should be blocked.
        let c = counter.clone();
        let result = RateLimiter::attempt(
            "a",
            3,
            || async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, FrameworkError>(())
            },
            60,
        )
        .await
        .unwrap();
        assert!(result.is_none());
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn increment_with_amount_bumps_by_n() {
        let _g = fresh_test_container();
        let n = RateLimiter::increment("bulk", 60, 5).await.unwrap();
        assert_eq!(n, 5);
        let n = RateLimiter::increment("bulk", 60, 3).await.unwrap();
        assert_eq!(n, 8);
        assert_eq!(RateLimiter::attempts("bulk").await.unwrap(), 8);
    }

    #[tokio::test]
    async fn decrement_subtracts_from_counter() {
        let _g = fresh_test_container();
        RateLimiter::increment("dec", 60, 10).await.unwrap();
        let n = RateLimiter::decrement("dec", 60, 3).await.unwrap();
        assert_eq!(n, 7);
    }

    #[test]
    fn named_limiter_define_then_resolve() {
        // Use a unique name so this test never races other tests.
        let name = "test:define:01";
        assert!(!RateLimiter::has_limiter(name));
        RateLimiter::define(name, |_req| Limit::per_minute(5).into());
        assert!(RateLimiter::has_limiter(name));
        let limiter = RateLimiter::limiter(name).unwrap();
        // Build a synthetic request to invoke the callback.
        // We don't have a constructed Request handy in unit tests; the
        // callback only needs `&Request`. We exercise the resolution
        // path; behavior is covered by the middleware integration test.
        assert!(Arc::strong_count(&limiter) >= 1);
    }

    #[test]
    fn r_for_is_alias_of_define() {
        let name = "test:rfor:02";
        RateLimiter::r#for(name, |_req| Limit::per_minute(10).into());
        assert!(RateLimiter::has_limiter(name));
    }

    /// The counter MUST age out with the window — `attempts(key)` must
    /// return 0 once the `:timer` has expired. On the in-memory backend
    /// this requires `CacheStore::increment` to preserve the seeded TTL
    /// (Redis `INCR` already does). Without that, both `attempts` and
    /// `remaining` lie after the window ends; `too_many_attempts` would
    /// still self-heal via the `:timer`, but `attempts` is documented to
    /// return 0 on expiry.
    #[tokio::test]
    async fn attempts_and_remaining_reflect_window_expiry() {
        let _g = fresh_test_container();
        let key = "expiry:check";
        // One-second decay so the test takes ~1.1s.
        RateLimiter::hit(key, 1).await.unwrap();
        RateLimiter::hit(key, 1).await.unwrap();
        assert_eq!(RateLimiter::attempts(key).await.unwrap(), 2);
        assert_eq!(RateLimiter::remaining(key, 5).await.unwrap(), 3);

        // Wait past the window. SystemTime + Instant both advance here —
        // tokio::time::sleep without start_paused sleeps real time.
        tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;

        assert_eq!(
            RateLimiter::attempts(key).await.unwrap(),
            0,
            "counter must age out with the window — both backends preserve TTL on increment"
        );
        assert_eq!(
            RateLimiter::remaining(key, 5).await.unwrap(),
            5,
            "remaining must read the post-expiry counter and return the full quota"
        );
        assert!(
            !RateLimiter::too_many_attempts(key, 2).await.unwrap(),
            "bucket reopens once the window expires"
        );
    }
}
