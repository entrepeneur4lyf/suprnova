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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;

use crate::FrameworkError;

pub use fake::{assert_not_sent, assert_sent, fake_response, RecordedRequest};

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

fn client() -> &'static reqwest::Client {
    REQWEST_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
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
/// The guard does NOT track previous state — `Drop` always returns
/// the flag to the default of "real calls allowed". If a test needs
/// to layer guards, it must coordinate explicitly via
/// [`Http::fail_on_real_calls`] / [`Http::allow_real_calls`].
#[must_use = "FailOnRealCallsGuard releases the guard on drop — bind it to a name"]
pub struct FailOnRealCallsGuard {
    _private: (),
}

impl FailOnRealCallsGuard {
    /// Flip [`Http::fail_on_real_calls`] on and return a guard whose
    /// `Drop` impl restores the default "real calls allowed" state.
    pub fn install() -> Self {
        Http::fail_on_real_calls();
        Self { _private: () }
    }
}

impl Drop for FailOnRealCallsGuard {
    fn drop(&mut self) {
        Http::allow_real_calls();
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
}

#[derive(Debug, Clone)]
pub(crate) enum Body {
    Json(serde_json::Value),
    Form(serde_json::Value),
    Raw(Bytes),
}

/// Retry policy attached to a [`RequestBuilder`].
///
/// Created with [`RequestBuilder::retry`]. Retries on transient
/// failures (connect/timeout, HTTP 5xx). The delay between attempt
/// `n` and attempt `n+1` is `base_backoff * 2^(n-1)`. For HTTP 503,
/// the larger of the computed backoff and any `Retry-After` header is
/// used.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryPolicy {
    pub(crate) max_attempts: u32,
    pub(crate) base_backoff: Duration,
}

/// Builder for an outbound HTTP request. Created via the [`Http`] facade.
pub struct RequestBuilder {
    pub(crate) method: Method,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Option<Body>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) retry: Option<RetryPolicy>,
}

impl RequestBuilder {
    pub(crate) fn new(method: Method, url: String) -> Self {
        Self {
            method,
            url,
            headers: Vec::new(),
            body: None,
            timeout: None,
            retry: None,
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
        let v = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        self.body = Some(Body::Json(v));
        self
    }

    /// Send the body as `application/x-www-form-urlencoded`. The value
    /// must serialize to a JSON object — keys become form fields.
    pub fn form<T: Serialize>(mut self, value: &T) -> Self {
        let v = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        self.body = Some(Body::Form(v));
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

    /// Attach a Bearer token via the `Authorization` header.
    pub fn bearer_token(self, token: impl AsRef<str>) -> Self {
        self.header("Authorization", format!("Bearer {}", token.as_ref()))
    }

    /// Attach HTTP Basic auth. `password` is optional — `None` produces
    /// `user:`.
    pub fn basic_auth(self, user: impl AsRef<str>, password: Option<&str>) -> Self {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let credential = format!("{}:{}", user.as_ref(), password.unwrap_or(""));
        let encoded = STANDARD.encode(credential);
        self.header("Authorization", format!("Basic {}", encoded))
    }

    /// Configure transient-failure retries.
    ///
    /// `max_attempts` is the total number of attempts including the
    /// first try (so `max_attempts=4` retries up to three times).
    /// `base_backoff` is the initial delay; subsequent delays double
    /// (100ms → 200 → 400 → 800ms for four attempts at 100ms base).
    ///
    /// A response is considered transient and eligible for retry if:
    /// - The send fails before we have a response (connect / timeout /
    ///   DNS errors).
    /// - The response status is 5xx (any server-side failure).
    ///
    /// Responses with 4xx are returned as-is — client errors are not
    /// retried. 2xx/3xx are returned as-is. After exhausting retries
    /// the last response (or the last error) is returned to the
    /// caller. For 503, the wait between attempts is the maximum of
    /// the computed backoff and the `Retry-After` header (parsed as a
    /// delta-seconds integer).
    ///
    /// Calling `.retry()` again replaces the previous policy.
    pub fn retry(mut self, max_attempts: u32, base_backoff: Duration) -> Self {
        let attempts = max_attempts.max(1);
        self.retry = Some(RetryPolicy {
            max_attempts: attempts,
            base_backoff,
        });
        self
    }

    /// Execute the request. When [`Http::fake`] is active, returns the
    /// matched canned response instead of hitting the network. If a
    /// retry policy is configured via [`Self::retry`], transient
    /// failures and 5xx responses are retried with exponential
    /// backoff (see [`Self::retry`] for the rules).
    pub async fn send(self) -> Result<ClientResponse, FrameworkError> {
        let policy = self.retry;
        let max_attempts = policy.map(|p| p.max_attempts).unwrap_or(1);

        let mut last_err: Option<FrameworkError> = None;
        for attempt in 1..=max_attempts {
            let outcome = if fake::is_fake_active() {
                Ok::<ClientResponse, FrameworkError>(fake::intercept(&self))
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
                    if is_transient && attempt < max_attempts
                        && let Some(p) = policy {
                            let backoff = backoff_for(attempt, p.base_backoff);
                            let wait = if status == 503 {
                                std::cmp::max(backoff, retry_after_from(&resp))
                            } else {
                                backoff
                            };
                            tokio::time::sleep(wait).await;
                            continue;
                        }
                    return Ok(resp);
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
    use opentelemetry::global;
    use crate::telemetry::propagation::HeaderInjector;

    let cx = opentelemetry::Context::current();
    let mut injector = HeaderInjector(request.headers_mut());
    global::get_text_map_propagator(|propagator| propagator.inject_context(&cx, &mut injector));
}

/// No-op stub when the `otel` feature is disabled — header injection
/// has nothing to do because no propagator is installed.
#[cfg(not(feature = "otel"))]
fn inject_w3c_trace_context(_request: &mut reqwest::Request) {}

/// `base_backoff * 2^(attempt-1)`. Saturating math so a pathologically
/// large attempt count can't overflow the shift, and the resulting
/// duration is clamped to ~136 years (`Duration::saturating_mul`).
fn backoff_for(attempt: u32, base_backoff: Duration) -> Duration {
    // `Duration::saturating_mul` takes u32; cap the exponent at 31 so
    // `1u32 << exp` is well-defined.
    let exp = attempt.saturating_sub(1).min(31);
    let factor: u32 = 1u32 << exp;
    base_backoff.saturating_mul(factor)
}

/// Parse a `Retry-After: <seconds>` header. HTTP-date form is not
/// supported here; integer delta-seconds is the only shape this honors.
/// Returns `Duration::ZERO` if missing or unparseable.
fn retry_after_from(resp: &ClientResponse) -> Duration {
    resp.header("Retry-After")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::ZERO)
}

/// Outbound response. Wraps `reqwest::Response` (real) or in-memory
/// bytes (fake).
pub struct ClientResponse(ClientResponseInner);

enum ClientResponseInner {
    Real(reqwest::Response),
    Fake { status: u16, headers: Vec<(String, String)>, body: Bytes },
}

impl ClientResponse {
    pub(crate) fn real(resp: reqwest::Response) -> Self {
        Self(ClientResponseInner::Real(resp))
    }

    pub(crate) fn fake(status: u16, headers: Vec<(String, String)>, body: Bytes) -> Self {
        Self(ClientResponseInner::Fake { status, headers, body })
    }

    /// Response status code.
    pub fn status(&self) -> u16 {
        match &self.0 {
            ClientResponseInner::Real(r) => r.status().as_u16(),
            ClientResponseInner::Fake { status, .. } => *status,
        }
    }

    /// Look up a response header by name. Case-insensitive.
    pub fn header(&self, name: &str) -> Option<String> {
        match &self.0 {
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

    /// Read the full body and parse as JSON.
    pub async fn json<T: serde::de::DeserializeOwned>(self) -> Result<T, FrameworkError> {
        match self.0 {
            ClientResponseInner::Real(r) => r
                .json()
                .await
                .map_err(|e| FrameworkError::internal(format!("Http json decode failed: {e}"))),
            ClientResponseInner::Fake { body, .. } => serde_json::from_slice(&body)
                .map_err(|e| FrameworkError::internal(format!("Http json decode failed: {e}"))),
        }
    }

    /// Read the full body as UTF-8 text.
    pub async fn text(self) -> Result<String, FrameworkError> {
        match self.0 {
            ClientResponseInner::Real(r) => r
                .text()
                .await
                .map_err(|e| FrameworkError::internal(format!("Http text decode failed: {e}"))),
            ClientResponseInner::Fake { body, .. } => String::from_utf8(body.to_vec())
                .map_err(|e| FrameworkError::internal(format!("Http body not UTF-8: {e}"))),
        }
    }

    /// Read the full body as bytes.
    pub async fn bytes(self) -> Result<Bytes, FrameworkError> {
        match self.0 {
            ClientResponseInner::Real(r) => r
                .bytes()
                .await
                .map_err(|e| FrameworkError::internal(format!("Http bytes failed: {e}"))),
            ClientResponseInner::Fake { body, .. } => Ok(body),
        }
    }

    /// Unwrap to the underlying `reqwest::Response`. This is an
    /// escape hatch for callers that need to reach for a `reqwest`
    /// API we don't expose (streaming bodies, redirect policy
    /// inspection, etc.).
    ///
    /// Returns `Err(FrameworkError::internal(...))` if the response
    /// was produced by [`Http::fake`] — there is no underlying
    /// `reqwest::Response` in that case. Real responses are returned
    /// via `Ok`.
    pub fn into_inner(self) -> Result<reqwest::Response, FrameworkError> {
        match self.0 {
            ClientResponseInner::Real(r) => Ok(r),
            ClientResponseInner::Fake { .. } => {
                Err(FrameworkError::internal(
                    "into_inner is not available on fake responses",
                ))
            }
        }
    }
}
