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

pub(crate) mod fake;

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use bytes::Bytes;
use serde::Serialize;

use crate::FrameworkError;

pub use fake::{assert_not_sent, assert_sent, fake_response, RecordedRequest};

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

    let resp = req
        .send()
        .await
        .map_err(|e| FrameworkError::internal(format!("Http::send failed: {e}")))?;
    Ok(ClientResponse::real(resp))
}

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
