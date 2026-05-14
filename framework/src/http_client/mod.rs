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
    /// Every `RequestBuilder::send` invoked from inside `f` (on the
    /// same task or any child task spawned via `tokio::spawn` that
    /// inherits this task-local) is intercepted: the request is
    /// captured, and a canned response queued via [`fake_response`] is
    /// returned. Tests in different tasks see different fake states,
    /// so parallel test execution is safe.
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

/// Builder for an outbound HTTP request. Created via the [`Http`] facade.
pub struct RequestBuilder {
    pub(crate) method: Method,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Option<Body>,
    pub(crate) timeout: Option<Duration>,
}

impl RequestBuilder {
    pub(crate) fn new(method: Method, url: String) -> Self {
        Self {
            method,
            url,
            headers: Vec::new(),
            body: None,
            timeout: None,
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

    /// Execute the request. When [`Http::fake`] is active, returns the
    /// matched canned response instead of hitting the network.
    pub async fn send(self) -> Result<ClientResponse, FrameworkError> {
        if fake::is_fake_active() {
            return Ok(fake::intercept(&self));
        }

        let mut req = client().request(self.method.into_reqwest(), &self.url);

        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if let Some(t) = self.timeout {
            req = req.timeout(t);
        }
        match self.body {
            Some(Body::Json(v)) => req = req.json(&v),
            Some(Body::Form(v)) => req = req.form(&v),
            Some(Body::Raw(bytes)) => req = req.body(bytes.to_vec()),
            None => {}
        }

        let resp = req
            .send()
            .await
            .map_err(|e| FrameworkError::internal(format!("Http::send failed: {e}")))?;
        Ok(ClientResponse::real(resp))
    }
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

    /// Unwrap to the underlying `reqwest::Response`. Panics if the
    /// response was produced by the fake recorder.
    pub fn into_inner(self) -> reqwest::Response {
        match self.0 {
            ClientResponseInner::Real(r) => r,
            ClientResponseInner::Fake { .. } => {
                panic!("ClientResponse::into_inner() called on a fake response")
            }
        }
    }
}
