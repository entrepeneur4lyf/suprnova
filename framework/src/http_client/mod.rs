//! Outbound HTTP client.
//!
//! `Http::get`, `Http::post`, etc. return a [`RequestBuilder`]; call
//! `.send().await` to execute and get a [`ClientResponse`]. The backing
//! `reqwest::Client` uses rustls for TLS, has a 30s default timeout, and
//! identifies itself with the `suprnova/<version>` user agent.
//!
//! For tests, [`Http::fake`] runs your async closure inside a
//! `tokio::task_local!` scope. Every outbound request from inside that
//! scope is captured into an in-memory recorder and answered with the
//! canned responses you've queued via `fake_response(...)`. Because the
//! state is task-local, tests can run in parallel without coordinating.
//!
//! Two additional helpers cover the corner where task-local isolation
//! is the wrong default:
//!
//! - [`Http::fail_on_real_calls`] flips a process-global guard so any
//!   outbound request that doesn't match an active fake errors out
//!   (with a `FrameworkError::internal` instead of hitting the
//!   network). This catches accidental escape from a fake scope —
//!   e.g. a `tokio::spawn` that doesn't inherit task-local state.
//!   Use [`FailOnRealCallsGuard`] for the RAII pattern that resets on
//!   drop, so a test forgetting to call `allow_real_calls` doesn't
//!   poison other tests.
//!
//! - [`Http::spawn_with_fake_inheritance`] is the explicit opt-in for
//!   spawning a task that inherits the parent's fake state. Recorded
//!   requests and consumed canned responses are shared with the
//!   parent — `assert_sent` on the parent sees what the child sent.

pub(crate) mod fake;

use std::future::Future;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;

use crate::FrameworkError;

pub use fake::{RecordedRequest, assert_not_sent, assert_sent, fake_response};

/// Process-global flag flipped by [`Http::fail_on_real_calls`]. When
/// `true`, [`RequestBuilder::send`] refuses to hit the real network
/// — every outbound call that isn't intercepted by an active fake
/// returns an error.
///
/// The flag is process-global by design: the goal is to fail closed on
/// accidental network escape from spawned tasks that don't inherit the
/// caller's task-local fake. Tests that flip this should use
/// [`FailOnRealCallsGuard`] (or call [`Http::allow_real_calls`] in
/// teardown) so the flag doesn't leak between tests.
static FAIL_ON_REAL_CALLS: AtomicBool = AtomicBool::new(false);

static REQWEST_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Default cap on a buffered outbound response body (25 MiB). A slow or
/// malicious upstream can otherwise stream an unbounded body into memory
/// via `ClientResponse::json`/`text`/`bytes`. Override globally with
/// [`Http::set_max_response_bytes`] or per request with
/// [`RequestBuilder::max_response_bytes`].
pub(crate) const DEFAULT_MAX_RESPONSE_BODY_BYTES: usize = 25 * 1024 * 1024;

/// Process-global response-body cap. `0` means "unset" — readers fall
/// back to [`DEFAULT_MAX_RESPONSE_BODY_BYTES`].
static MAX_RESPONSE_BODY_BYTES: AtomicUsize = AtomicUsize::new(0);

fn client() -> &'static reqwest::Client {
    REQWEST_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("suprnova/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest::Client::builder().build() — rustls available")
    })
}

/// Static facade for outbound HTTP requests. Closed for v1 — we do not
/// expose the underlying `reqwest::Client`. To grow the surface, add
/// methods here.
pub struct Http;

impl Http {
    /// Begin a GET request.
    pub fn get(url: impl Into<String>) -> RequestBuilder {
        RequestBuilder::new(Method::Get, url.into())
    }

    /// Begin a POST request.
    pub fn post(url: impl Into<String>) -> RequestBuilder {
        RequestBuilder::new(Method::Post, url.into())
    }

    /// Begin a PUT request.
    pub fn put(url: impl Into<String>) -> RequestBuilder {
        RequestBuilder::new(Method::Put, url.into())
    }

    /// Begin a PATCH request.
    pub fn patch(url: impl Into<String>) -> RequestBuilder {
        RequestBuilder::new(Method::Patch, url.into())
    }

    /// Begin a DELETE request.
    pub fn delete(url: impl Into<String>) -> RequestBuilder {
        RequestBuilder::new(Method::Delete, url.into())
    }

    /// Run an async test body inside a fake-HTTP scope.
    ///
    /// Every `RequestBuilder::send` invoked from inside `f` is
    /// intercepted: the request is captured, and a canned response
    /// queued via [`fake_response`] is returned. Tests in different
    /// tasks see different fake states, so parallel test execution is
    /// safe.
    ///
    /// **Caveat:** the scope is `tokio::task_local!`, which is scoped
    /// to the *current* task only. Work spawned via `tokio::spawn`
    /// inside `f` runs on a different task and will NOT see the fake
    /// — those requests will hit the real network. If you need a
    /// spawned task to use the fake, capture the work into a closure
    /// and `await` it directly, or pass the fake scope's data through
    /// explicit channels.
    ///
    /// Returns whatever the closure returns.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use suprnova::{Http, fake_response, assert_sent};
    ///
    /// Http::fake(|| async {
    ///     fake_response("POST", "/api/users", 201, serde_json::json!({"id": 1}));
    ///     let resp = Http::post("https://example.com/api/users")
    ///         .json(&serde_json::json!({"name": "Ada"}))
    ///         .send()
    ///         .await
    ///         .unwrap();
    ///     assert_eq!(resp.status(), 201);
    ///     assert_sent(|r| r.method == "POST" && r.url.contains("/api/users"));
    /// })
    /// .await;
    /// ```
    pub async fn fake<F, Fut, T>(f: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        fake::install_fake_scope(f).await
    }

    /// Enable test-guard mode: any outbound HTTP call that doesn't
    /// match an active fake returns
    /// `Err(FrameworkError::internal(...))` instead of hitting the
    /// real network.
    ///
    /// This is intentionally process-global so it catches the case it
    /// was built for — `tokio::spawn`-ed work escaping the caller's
    /// task-local [`Http::fake`] scope and silently calling real
    /// services. Pair with [`Self::allow_real_calls`] in teardown, or
    /// prefer [`FailOnRealCallsGuard`] which resets on drop:
    ///
    /// ```rust,ignore
    /// let _guard = suprnova::FailOnRealCallsGuard::install();
    /// // Inside this scope, any unfaked outbound call fails closed.
    /// ```
    pub fn fail_on_real_calls() {
        FAIL_ON_REAL_CALLS.store(true, Ordering::SeqCst);
    }

    /// Disable the test-guard mode. After this returns, unfaked
    /// outbound calls proceed to the real network as usual. Default
    /// state at process start is "real calls allowed".
    pub fn allow_real_calls() {
        FAIL_ON_REAL_CALLS.store(false, Ordering::SeqCst);
    }

    /// `true` when [`Self::fail_on_real_calls`] is active.
    pub fn is_guarded() -> bool {
        FAIL_ON_REAL_CALLS.load(Ordering::SeqCst)
    }

    /// Set the process-global cap on a buffered outbound response body
    /// (`ClientResponse::json`/`text`/`bytes`). Bounds memory pressure
    /// from a slow or malicious upstream streaming a very large body.
    /// Set once at boot; per-request overrides via
    /// [`RequestBuilder::max_response_bytes`].
    pub fn set_max_response_bytes(limit: usize) {
        MAX_RESPONSE_BODY_BYTES.store(limit, Ordering::SeqCst);
    }

    /// The effective process-global response-body cap — the value set by
    /// [`Self::set_max_response_bytes`], or
    /// `DEFAULT_MAX_RESPONSE_BODY_BYTES` (25 MiB) if unset.
    pub fn max_response_bytes() -> usize {
        match MAX_RESPONSE_BODY_BYTES.load(Ordering::SeqCst) {
            0 => DEFAULT_MAX_RESPONSE_BODY_BYTES,
            n => n,
        }
    }

    /// Spawn a task that inherits the calling task's fake state.
    ///
    /// `tokio::spawn` does NOT carry `tokio::task_local!` values into
    /// the spawned future, so a fake registered in the outer scope
    /// doesn't apply to the spawned task. This helper captures the
    /// current task's fake state (an `Arc<Mutex<FakeState>>`) and
    /// re-installs it in the child's task-local scope. Recorded
    /// requests from the child are visible to the parent through the
    /// same Arc — `assert_sent` after the child completes sees what
    /// the child sent.
    ///
    /// Most production code shouldn't need this — it's a test-time
    /// helper for code-under-test that itself spawns tasks (e.g. a
    /// queue worker that makes outbound HTTP from a spawned future).
    ///
    /// If no fake scope is active on the calling task, this is
    /// equivalent to `tokio::spawn(future)` — the child runs without
    /// any fake context and outbound calls take the normal real
    /// network path (or fail closed when
    /// [`Self::fail_on_real_calls`] is on).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Http::fake(|| async {
    ///     fake_response("GET", "/child", 204, serde_json::json!({}));
    ///     let handle = Http::spawn_with_fake_inheritance(async {
    ///         Http::get("https://child.test").send().await
    ///     });
    ///     let response = handle.await.unwrap().unwrap();
    ///     assert_eq!(response.status(), 204);
    /// })
    /// .await;
    /// ```
    pub fn spawn_with_fake_inheritance<F, T>(future: F) -> tokio::task::JoinHandle<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        match fake::snapshot_current_fake_state() {
            Some(state) => {
                tokio::spawn(async move { fake::install_inherited_scope(state, future).await })
            }
            None => tokio::spawn(future),
        }
    }
}

/// RAII guard for [`Http::fail_on_real_calls`]. `install()` flips the
/// flag on; the guard's `Drop` impl flips it back off, even if the
/// test panics. Use this in test setup to avoid poisoning sibling
/// tests when the body exits early:
///
/// ```rust,ignore
/// #[tokio::test]
/// async fn my_test() {
///     let _guard = suprnova::FailOnRealCallsGuard::install();
///     // Any unfaked outbound HTTP call in this scope errors out.
/// }
/// ```
///
/// `Drop` restores the state that was in effect when the guard was
/// installed, so nested guards compose correctly: dropping an inner guard
/// returns the flag to whatever the outer scope had set, not
/// unconditionally to "allowed".
///
/// # Parallel-task caveat
///
/// The underlying flag is a process-global `AtomicBool`, so parallel
/// tasks that each install their own guard race on the same cell — an
/// inner guard from one task can briefly relax the guard for another
/// task that expects it to stay on. Process-global by design: this
/// catches the exact failure it was built for (work `tokio::spawn`-ed
/// out of a [`Http::fake`] scope hitting the real network). For
/// parallel test isolation, prefer per-task fake scopes via
/// [`Http::fake`] + [`Http::spawn_with_fake_inheritance`] instead of
/// relying on the guard alone.
#[must_use = "FailOnRealCallsGuard releases the guard on drop — bind it to a name"]
pub struct FailOnRealCallsGuard {
    previous: bool,
}

impl FailOnRealCallsGuard {
    /// Flip [`Http::fail_on_real_calls`] on and return a guard whose
    /// `Drop` impl restores the PREVIOUS state (not unconditionally
    /// "allowed"), making nested installs safe.
    pub fn install() -> Self {
        let previous = Http::is_guarded();
        Http::fail_on_real_calls();
        Self { previous }
    }
}

impl Drop for FailOnRealCallsGuard {
    fn drop(&mut self) {
        FAIL_ON_REAL_CALLS.store(self.previous, Ordering::SeqCst);
    }
}

/// HTTP method, kept as a small internal enum so we don't leak
/// `reqwest::Method` through our API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    fn into_reqwest(self) -> reqwest::Method {
        match self {
            Self::Get => reqwest::Method::GET,
            Self::Post => reqwest::Method::POST,
            Self::Put => reqwest::Method::PUT,
            Self::Patch => reqwest::Method::PATCH,
            Self::Delete => reqwest::Method::DELETE,
        }
    }

    /// Whether this method is idempotent per RFC 7231 §4.2.2 — sending
    /// the request more than once has the same effect as sending it once.
    /// Retries are only safe (no duplicated side effect) for idempotent
    /// methods. GET/PUT/DELETE are idempotent; POST and PATCH are not.
    pub(crate) fn is_idempotent(self) -> bool {
        matches!(self, Self::Get | Self::Put | Self::Delete)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Body {
    Json(serde_json::Value),
    Form(serde_json::Value),
    Raw(Bytes),
}

/// Retry policy attached to a [`RequestBuilder`].
///
/// Created with [`RequestBuilder::retry`] (idempotent methods only) or
/// [`RequestBuilder::retry_non_idempotent`] (all methods). Retries on
/// transient failures (connect/timeout, HTTP 5xx). The delay before
/// attempt `n+1` is a random duration in `[0, base_backoff * 2^(n-1)]`
/// (full jitter), capped at 30s. For HTTP 503 the wait is the larger of
/// that backoff and any `Retry-After` header, still capped at 30s.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryPolicy {
    pub(crate) max_attempts: u32,
    pub(crate) base_backoff: Duration,
    /// When `false` (the default, set by [`RequestBuilder::retry`]),
    /// retries are limited to idempotent methods. When `true` (set by
    /// [`RequestBuilder::retry_non_idempotent`]), POST/PATCH are retried
    /// too — only safe when the upstream is protected by an idempotency
    /// key or is otherwise safe to call more than once.
    pub(crate) retry_non_idempotent: bool,
}

/// Builder for an outbound HTTP request. Created via the [`Http`] facade.
pub struct RequestBuilder {
    pub(crate) method: Method,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Option<Body>,
    /// Set when [`RequestBuilder::json`]/[`RequestBuilder::form`] fail to
    /// serialize the value. [`RequestBuilder::send`] surfaces it as an
    /// error instead of sending a body that silently degraded to `null`.
    pub(crate) body_error: Option<String>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) retry: Option<RetryPolicy>,
    /// Per-request response-body cap; falls back to the process-global
    /// default ([`Http::max_response_bytes`]) when `None`.
    pub(crate) max_response_bytes: Option<usize>,
}

impl RequestBuilder {
    pub(crate) fn new(method: Method, url: String) -> Self {
        Self {
            method,
            url,
            headers: Vec::new(),
            body: None,
            body_error: None,
            timeout: None,
            retry: None,
            max_response_bytes: None,
        }
    }

    /// Append a header. Repeats are kept; reqwest will join them per
    /// HTTP semantics.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Send the body as JSON. Replaces any previously-set body. Sets
    /// `Content-Type: application/json` automatically on the wire.
    pub fn json<T: Serialize>(mut self, value: &T) -> Self {
        match serde_json::to_value(value) {
            Ok(v) => self.body = Some(Body::Json(v)),
            // Record the failure rather than silently sending `null`;
            // `send` turns this into an error before any request goes out.
            Err(e) => self.body_error = Some(format!("Http::json body serialization failed: {e}")),
        }
        self
    }

    /// Send the body as `application/x-www-form-urlencoded`. The value
    /// must serialize to a JSON object — keys become form fields.
    pub fn form<T: Serialize>(mut self, value: &T) -> Self {
        match serde_json::to_value(value) {
            Ok(v) => self.body = Some(Body::Form(v)),
            Err(e) => self.body_error = Some(format!("Http::form body serialization failed: {e}")),
        }
        self
    }

    /// Send a raw byte body. The caller is responsible for setting
    /// `Content-Type`.
    pub fn body(mut self, bytes: impl Into<Bytes>) -> Self {
        self.body = Some(Body::Raw(bytes.into()));
        self
    }

    /// Override the request timeout. Defaults to 30 seconds from the
    /// shared client.
    pub fn timeout(mut self, dur: Duration) -> Self {
        self.timeout = Some(dur);
        self
    }

    /// Cap the response body this request will buffer (via
    /// `ClientResponse::json`/`text`/`bytes`), overriding the
    /// process-global [`Http::max_response_bytes`] for this one request.
    pub fn max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = Some(limit);
        self
    }

    /// Attach a Bearer token via the `Authorization` header.
    pub fn bearer_token(self, token: impl AsRef<str>) -> Self {
        self.header("Authorization", format!("Bearer {}", token.as_ref()))
    }

    /// Attach HTTP Basic auth. `password` is optional — `None` produces
    /// `user:`.
    pub fn basic_auth(self, user: impl AsRef<str>, password: Option<&str>) -> Self {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let credential = format!("{}:{}", user.as_ref(), password.unwrap_or(""));
        let encoded = STANDARD.encode(credential);
        self.header("Authorization", format!("Basic {}", encoded))
    }

    /// Configure transient-failure retries for IDEMPOTENT methods.
    ///
    /// `max_attempts` is the total number of attempts including the
    /// first try (so `max_attempts=4` retries up to three times).
    /// `base_backoff` seeds the delay; the wait before attempt `n+1` is a
    /// random duration in `[0, base_backoff * 2^(n-1)]` (full jitter, so
    /// many workers retrying the same outage don't synchronize into a
    /// thundering herd), capped at 30s.
    ///
    /// A request is eligible for retry only if its method is idempotent
    /// (GET/PUT/DELETE) — see [`Self::retry_non_idempotent`] to opt POST/
    /// PATCH in. An eligible request is retried when:
    /// - The send fails before we have a response (connect / timeout /
    ///   DNS errors), or
    /// - The response status is 5xx.
    ///
    /// 4xx and 2xx/3xx are returned as-is. After exhausting retries the
    /// last response (or the last error) is returned. For 503 the wait is
    /// the larger of the jittered backoff and the `Retry-After` header
    /// (delta-seconds or HTTP-date), still capped at 30s.
    ///
    /// Calling `.retry()` again replaces the previous policy.
    pub fn retry(mut self, max_attempts: u32, base_backoff: Duration) -> Self {
        let attempts = max_attempts.max(1);
        self.retry = Some(RetryPolicy {
            max_attempts: attempts,
            base_backoff,
            retry_non_idempotent: false,
        });
        self
    }

    /// Like [`Self::retry`], but ALSO retries non-idempotent methods
    /// (`POST`, `PATCH`).
    ///
    /// [`Self::retry`] deliberately skips POST/PATCH: if the upstream
    /// already performed the write but the response was lost (or it
    /// returned 5xx *after* committing), a blind retry duplicates the
    /// side effect. Only reach for this when the request is safe to send
    /// more than once — e.g. it carries an idempotency key the server
    /// honors, or the operation is naturally safe to repeat. Idempotent
    /// methods (GET/PUT/DELETE) are retried by both this and
    /// [`Self::retry`]; calling either again replaces the previous policy.
    pub fn retry_non_idempotent(mut self, max_attempts: u32, base_backoff: Duration) -> Self {
        self.retry = Some(RetryPolicy {
            max_attempts: max_attempts.max(1),
            base_backoff,
            retry_non_idempotent: true,
        });
        self
    }

    /// Execute the request. When [`Http::fake`] is active, returns the
    /// matched canned response instead of hitting the network. If a
    /// retry policy is configured via [`Self::retry`], transient
    /// failures and 5xx responses are retried with exponential
    /// backoff (see [`Self::retry`] for the rules).
    pub async fn send(self) -> Result<ClientResponse, FrameworkError> {
        // Surface a json()/form() serialization failure recorded on the
        // builder instead of sending a body that silently degraded to null.
        if let Some(err) = &self.body_error {
            return Err(FrameworkError::internal(err.clone()));
        }
        // Cap applied to whatever body the returned response buffers.
        let effective_max = self
            .max_response_bytes
            .unwrap_or_else(Http::max_response_bytes);

        let policy = self.retry;
        let max_attempts = policy.map(|p| p.max_attempts).unwrap_or(1);
        // A request is eligible for retry only when a policy is set AND
        // either the method is idempotent or the caller explicitly opted
        // non-idempotent methods in. This prevents a blind replay of a
        // POST/PATCH whose first attempt may have already taken effect.
        let method_retryable = policy
            .map(|p| p.retry_non_idempotent || self.method.is_idempotent())
            .unwrap_or(false);

        let mut last_err: Option<FrameworkError> = None;
        for attempt in 1..=max_attempts {
            let outcome = if fake::is_fake_active() {
                fake::intercept(&self)
            } else if Http::is_guarded() {
                // Process-global fail-closed mode: outbound calls
                // that don't match an active fake error out instead
                // of hitting the real network. Mirrors Laravel's
                // `Http::preventStrayRequests()`. The error is
                // `FrameworkError::internal` — the request URL is
                // included so the user can identify where the
                // unmatched call originated, but no headers/body
                // detail leaks.
                Err::<ClientResponse, FrameworkError>(FrameworkError::internal(format!(
                    "Http::fail_on_real_calls is active and no fake matched outbound \
                     request to {}. Register a matching fake via fake_response(...), \
                     or release the guard via FailOnRealCallsGuard / \
                     Http::allow_real_calls() to allow real network access.",
                    self.url,
                )))
            } else {
                build_and_send(&self).await
            };

            match outcome {
                Ok(resp) => {
                    let status = resp.status();
                    let is_transient = (500..600).contains(&status);
                    if is_transient
                        && method_retryable
                        && attempt < max_attempts
                        && let Some(p) = policy
                    {
                        let backoff = backoff_for(attempt, p.base_backoff);
                        let wait = if status == 503 {
                            std::cmp::min(
                                std::cmp::max(backoff, retry_after_from(&resp)),
                                MAX_RETRY_WAIT,
                            )
                        } else {
                            backoff
                        };
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    return Ok(resp.with_max_bytes(effective_max));
                }
                Err(e) => {
                    if let Some(p) = policy.filter(|_| attempt < max_attempts) {
                        let backoff = backoff_for(attempt, p.base_backoff);
                        last_err = Some(e);
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            FrameworkError::internal("Http::send retries exhausted without a response")
        }))
    }
}

/// Single attempt at the request. No retry logic, no fake interception.
async fn build_and_send(builder: &RequestBuilder) -> Result<ClientResponse, FrameworkError> {
    let mut req = client().request(builder.method.into_reqwest(), &builder.url);

    for (k, v) in &builder.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(t) = builder.timeout {
        req = req.timeout(t);
    }
    match &builder.body {
        Some(Body::Json(v)) => req = req.json(v),
        Some(Body::Form(v)) => req = req.form(v),
        Some(Body::Raw(bytes)) => req = req.body(bytes.to_vec()),
        None => {}
    }

    // Build the request so we can mutate its header map to inject
    // W3C trace context (otel feature only — no-op otherwise).
    let mut request = req
        .build()
        .map_err(|e| FrameworkError::internal(format!("Http::send failed: {e}")))?;

    inject_w3c_trace_context(&mut request);

    let resp = client()
        .execute(request)
        .await
        .map_err(|e| FrameworkError::internal(format!("Http::send failed: {e}")))?;
    Ok(ClientResponse::real(resp))
}

/// Inject the current OpenTelemetry context into outbound request
/// headers using the globally-registered text-map propagator
/// (`TraceContextPropagator` is installed by `init_telemetry` when the
/// `otel` feature is enabled). Produces `traceparent` / `tracestate`
/// headers that downstream services parse to continue the trace.
///
/// If no OTel context is active (i.e. `Context::current()` is empty),
/// the propagator emits nothing and headers are left untouched. This
/// keeps the code path safe to run unconditionally on every request.
#[cfg(feature = "otel")]
fn inject_w3c_trace_context(request: &mut reqwest::Request) {
    use crate::telemetry::propagation::HeaderInjector;
    use opentelemetry::global;

    let cx = opentelemetry::Context::current();
    let mut injector = HeaderInjector(request.headers_mut());
    global::get_text_map_propagator(|propagator| propagator.inject_context(&cx, &mut injector));
}

/// No-op stub when the `otel` feature is disabled — header injection
/// has nothing to do because no propagator is installed.
#[cfg(not(feature = "otel"))]
fn inject_w3c_trace_context(_request: &mut reqwest::Request) {}

/// Maximum wait between two retry attempts. Bounds both the exponential
/// backoff and a hostile `Retry-After` (e.g. `Retry-After: 86400`) so a
/// single retry can never park a task for more than 30 seconds.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);

/// Exponential backoff with full jitter. The ceiling is
/// `base_backoff * 2^(attempt-1)` (saturating, capped at
/// [`MAX_RETRY_WAIT`]); the returned wait is a uniform random duration in
/// `[0, ceiling]`. Full jitter (AWS's published recipe) keeps many
/// workers retrying the same outage from synchronizing into a thundering
/// herd.
fn backoff_for(attempt: u32, base_backoff: Duration) -> Duration {
    use rand::RngExt;

    // `Duration::saturating_mul` takes u32; cap the exponent at 31 so
    // `1u32 << exp` is well-defined.
    let exp = attempt.saturating_sub(1).min(31);
    let factor: u32 = 1u32 << exp;
    let ceiling = base_backoff.saturating_mul(factor).min(MAX_RETRY_WAIT);
    let ceiling_ms = ceiling.as_millis() as u64;
    if ceiling_ms == 0 {
        return Duration::ZERO;
    }
    // Uniform in `[0, ceiling_ms]`; millisecond precision is plenty for
    // backoff scheduling.
    let jittered = rand::rng().random_range(0..=ceiling_ms);
    Duration::from_millis(jittered)
}

/// Parse a `Retry-After` header in either RFC 7231 form: integer
/// delta-seconds, or an HTTP-date. For an HTTP-date the wait is the time
/// from now until that instant (a date already in the past yields
/// `Duration::ZERO`). Returns `Duration::ZERO` if the header is missing
/// or unparseable.
fn retry_after_from(resp: &ClientResponse) -> Duration {
    let Some(raw) = resp.header("Retry-After") else {
        return Duration::ZERO;
    };
    let raw = raw.trim();
    // Delta-seconds form (the common case).
    if let Ok(secs) = raw.parse::<u64>() {
        return Duration::from_secs(secs);
    }
    // HTTP-date form: wait until that instant, clamped at zero if it is
    // already in the past.
    match httpdate::parse_http_date(raw) {
        Ok(when) => when
            .duration_since(std::time::SystemTime::now())
            .unwrap_or(Duration::ZERO),
        Err(_) => Duration::ZERO,
    }
}

/// Outbound response. Wraps `reqwest::Response` (real) or in-memory
/// bytes (fake). `max_bytes` caps how much body `json`/`text`/`bytes`
/// will buffer.
pub struct ClientResponse {
    inner: ClientResponseInner,
    max_bytes: usize,
}

enum ClientResponseInner {
    Real(reqwest::Response),
    Fake {
        status: u16,
        headers: Vec<(String, String)>,
        body: Bytes,
    },
}

impl ClientResponse {
    pub(crate) fn real(resp: reqwest::Response) -> Self {
        Self {
            inner: ClientResponseInner::Real(resp),
            max_bytes: DEFAULT_MAX_RESPONSE_BODY_BYTES,
        }
    }

    pub(crate) fn fake(status: u16, headers: Vec<(String, String)>, body: Bytes) -> Self {
        Self {
            inner: ClientResponseInner::Fake {
                status,
                headers,
                body,
            },
            max_bytes: DEFAULT_MAX_RESPONSE_BODY_BYTES,
        }
    }

    /// Set the response-body cap. Called by [`RequestBuilder::send`] with
    /// the request's effective limit.
    pub(crate) fn with_max_bytes(mut self, max: usize) -> Self {
        self.max_bytes = max;
        self
    }

    /// Response status code.
    pub fn status(&self) -> u16 {
        match &self.inner {
            ClientResponseInner::Real(r) => r.status().as_u16(),
            ClientResponseInner::Fake { status, .. } => *status,
        }
    }

    /// Look up a response header by name. Case-insensitive.
    pub fn header(&self, name: &str) -> Option<String> {
        match &self.inner {
            ClientResponseInner::Real(r) => r
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok().map(|s| s.to_string())),
            ClientResponseInner::Fake { headers, .. } => headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone()),
        }
    }

    /// Read the full body and parse as JSON, enforcing the response-body
    /// cap (see [`Http::set_max_response_bytes`] /
    /// [`RequestBuilder::max_response_bytes`]).
    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, FrameworkError> {
        let max = self.max_bytes;
        let bytes = match self.inner {
            ClientResponseInner::Real(r) => read_capped(r, max).await?,
            ClientResponseInner::Fake { body, .. } => check_fake_within_cap(body, max)?,
        };
        serde_json::from_slice(&bytes)
            .map_err(|e| FrameworkError::internal(format!("Http json decode failed: {e}")))
    }

    /// Read the full body as UTF-8 text, enforcing the response-body cap.
    pub async fn text(self) -> Result<String, FrameworkError> {
        let max = self.max_bytes;
        let bytes = match self.inner {
            ClientResponseInner::Real(r) => read_capped(r, max).await?,
            ClientResponseInner::Fake { body, .. } => check_fake_within_cap(body, max)?,
        };
        String::from_utf8(bytes.to_vec())
            .map_err(|e| FrameworkError::internal(format!("Http body not UTF-8: {e}")))
    }

    /// Read the full body as bytes, enforcing the response-body cap.
    pub async fn bytes(self) -> Result<Bytes, FrameworkError> {
        let max = self.max_bytes;
        match self.inner {
            ClientResponseInner::Real(r) => read_capped(r, max).await,
            ClientResponseInner::Fake { body, .. } => check_fake_within_cap(body, max),
        }
    }

    /// Unwrap to the underlying `reqwest::Response`. This is an
    /// escape hatch for callers that need to reach for a `reqwest`
    /// API we don't expose (streaming bodies, redirect policy
    /// inspection, etc.). The response-body cap does NOT apply once you
    /// take the raw response — you own the read from there.
    ///
    /// Returns `Err(FrameworkError::internal(...))` if the response
    /// was produced by [`Http::fake`] — there is no underlying
    /// `reqwest::Response` in that case. Real responses are returned
    /// via `Ok`.
    pub fn into_inner(self) -> Result<reqwest::Response, FrameworkError> {
        match self.inner {
            ClientResponseInner::Real(r) => Ok(r),
            ClientResponseInner::Fake { .. } => Err(FrameworkError::internal(
                "into_inner is not available on fake responses",
            )),
        }
    }
}

/// Buffer a reqwest response body, rejecting it once it exceeds `max`
/// bytes. A declared `Content-Length` over the cap is rejected before any
/// body is read; the streaming loop then enforces the cap against the
/// actual bytes (Content-Length can be absent or lie).
async fn read_capped(resp: reqwest::Response, max: usize) -> Result<Bytes, FrameworkError> {
    if let Some(len) = resp.content_length()
        && len > max as u64
    {
        return Err(FrameworkError::internal(format!(
            "Http response body exceeds the {max}-byte cap (Content-Length {len})"
        )));
    }
    let mut resp = resp;
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| FrameworkError::internal(format!("Http body read failed: {e}")))?
    {
        if buf.len() + chunk.len() > max {
            return Err(FrameworkError::internal(format!(
                "Http response body exceeds the {max}-byte cap"
            )));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(buf))
}

/// Enforce the response-body cap on an in-memory fake body.
fn check_fake_within_cap(body: Bytes, max: usize) -> Result<Bytes, FrameworkError> {
    if body.len() > max {
        return Err(FrameworkError::internal(format!(
            "Http response body exceeds the {max}-byte cap"
        )));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotent_methods_are_get_put_delete() {
        assert!(Method::Get.is_idempotent());
        assert!(Method::Put.is_idempotent());
        assert!(Method::Delete.is_idempotent());
        assert!(!Method::Post.is_idempotent());
        assert!(!Method::Patch.is_idempotent());
    }

    #[test]
    fn backoff_stays_within_ceiling_and_is_capped() {
        // Full jitter: every sample sits in [0, ceiling]. With a 100ms
        // base, attempt 3's ceiling is 100ms * 2^2 = 400ms.
        let base = Duration::from_millis(100);
        for _ in 0..256 {
            assert!(
                backoff_for(3, base) <= Duration::from_millis(400),
                "jittered backoff exceeded its ceiling"
            );
        }
        // A pathologically large attempt / base is bounded by the 30s cap
        // rather than overflowing or parking for longer.
        for _ in 0..256 {
            assert!(
                backoff_for(40, Duration::from_secs(10)) <= MAX_RETRY_WAIT,
                "backoff exceeded the 30s cap"
            );
        }
    }

    #[test]
    fn retry_after_parses_delta_seconds_and_http_date() {
        let with_header = |value: String| {
            ClientResponse::fake(503, vec![("Retry-After".to_string(), value)], Bytes::new())
        };

        // Delta-seconds form.
        assert_eq!(
            retry_after_from(&with_header("5".to_string())),
            Duration::from_secs(5)
        );

        // HTTP-date ~3s in the future parses to roughly 3s (HTTP-date has
        // whole-second granularity, so allow generous slack).
        let future = std::time::SystemTime::now() + Duration::from_secs(3);
        let d = retry_after_from(&with_header(httpdate::fmt_http_date(future)));
        assert!(
            d >= Duration::from_secs(1) && d <= Duration::from_secs(3),
            "http-date Retry-After should be ~3s, got {d:?}"
        );

        // A past HTTP-date clamps to zero.
        let past = std::time::SystemTime::now() - Duration::from_secs(120);
        assert_eq!(
            retry_after_from(&with_header(httpdate::fmt_http_date(past))),
            Duration::ZERO
        );

        // Unparseable header → zero.
        assert_eq!(
            retry_after_from(&with_header("soon".to_string())),
            Duration::ZERO
        );
    }
}
