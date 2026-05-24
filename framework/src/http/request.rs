use super::body::{collect_body_with_cap, global_max_request_body_bytes, parse_form, parse_json};
use super::cookie::parse_cookies;
use super::ParamError;
use crate::error::FrameworkError;
use bytes::Bytes;
use serde::de::DeserializeOwned;
use std::collections::HashMap;

/// State of the request body — either still streaming from the wire,
/// already buffered into memory, or fully consumed.
///
/// Buffering happens when middleware needs to inspect the body and
/// still hand the request to downstream handlers (e.g. the CSRF
/// middleware reading a `_token` form field). Once buffered, subsequent
/// `body_bytes` / `form` / `json` reads return the cached bytes without
/// touching the underlying stream.
pub enum BodyState {
    /// Body has not been read yet — still a streaming hyper body.
    Streaming(hyper::body::Incoming),
    /// Body was collected into memory. Subsequent reads return these bytes.
    Buffered(Bytes),
    /// Body was consumed without buffering. Subsequent reads error.
    Consumed,
}

/// HTTP Request wrapper providing Laravel-like access to request data.
///
/// The body is held in a [`BodyState`] enum so middleware can buffer it
/// for inspection (via [`Request::buffer_body`]) and downstream
/// handlers can still read the original bytes.
pub struct Request {
    parts: hyper::http::request::Parts,
    body: BodyState,
    params: HashMap<String, String>,
}

impl Request {
    pub fn new(inner: hyper::Request<hyper::body::Incoming>) -> Self {
        let (parts, body) = inner.into_parts();
        Self {
            parts,
            body: BodyState::Streaming(body),
            params: HashMap::new(),
        }
    }

    pub fn with_params(mut self, params: HashMap<String, String>) -> Self {
        self.params = params;
        self
    }

    /// Get the request method
    pub fn method(&self) -> &hyper::Method {
        &self.parts.method
    }

    /// Get the request path
    pub fn path(&self) -> &str {
        self.parts.uri.path()
    }

    /// Returns the query string portion of the request URI (the part
    /// after `?`), or `None` when no query is present.
    pub fn query(&self) -> Option<&str> {
        self.parts.uri.query()
    }

    /// Get the request URI
    pub fn uri(&self) -> &hyper::Uri {
        &self.parts.uri
    }

    /// Get the full request headers map (read-only).
    pub fn headers(&self) -> &hyper::HeaderMap {
        &self.parts.headers
    }

    /// Get a route parameter by name (e.g., /users/{id})
    /// Returns Err(ParamError) if the parameter is missing, enabling use of `?` operator
    pub fn param(&self, name: &str) -> Result<&str, ParamError> {
        self.params
            .get(name)
            .map(|s| s.as_str())
            .ok_or_else(|| ParamError {
                param_name: name.to_string(),
            })
    }

    /// Get all route parameters
    pub fn params(&self) -> &HashMap<String, String> {
        &self.params
    }

    /// Return a clone of all route parameters as an owned map.
    ///
    /// Used by the `#[data(from_route_param)]` generated code, which must
    /// snapshot the params before consuming `self` via `body_bytes()`.
    pub fn all_route_params(&self) -> HashMap<String, String> {
        self.params.clone()
    }

    /// Get a header value by name
    pub fn header(&self, name: &str) -> Option<&str> {
        self.parts.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// Get the Content-Type header
    pub fn content_type(&self) -> Option<&str> {
        self.header("content-type")
    }

    /// Check if this is an Inertia XHR request
    pub fn is_inertia(&self) -> bool {
        self.header("X-Inertia")
            .map(|v| v == "true")
            .unwrap_or(false)
    }

    /// Get all cookies from the request
    ///
    /// Parses the Cookie header and returns a HashMap of cookie names to values.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let cookies = req.cookies();
    /// if let Some(session) = cookies.get("session") {
    ///     println!("Session: {}", session);
    /// }
    /// ```
    pub fn cookies(&self) -> HashMap<String, String> {
        self.header("Cookie")
            .map(parse_cookies)
            .unwrap_or_default()
    }

    /// Get a specific cookie value by name
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if let Some(session_id) = req.cookie("session") {
    ///     // Use session_id
    /// }
    /// ```
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.cookies().get(name).cloned()
    }

    /// Get the Inertia version from request headers
    pub fn inertia_version(&self) -> Option<&str> {
        self.header("X-Inertia-Version")
    }

    /// Get partial component name for partial reloads
    pub fn inertia_partial_component(&self) -> Option<&str> {
        self.header("X-Inertia-Partial-Component")
    }

    /// Get partial data keys for partial reloads
    pub fn inertia_partial_data(&self) -> Option<Vec<&str>> {
        self.header("X-Inertia-Partial-Data")
            .map(|v| v.split(',').collect())
    }

    /// Consume the request and collect the body as bytes, capped at the
    /// process-global request-body limit.
    ///
    /// See [`crate::http::body::global_max_request_body_bytes`] and
    /// [`crate::http::body::set_global_max_request_body_bytes`] for tuning
    /// the cap. For per-extractor overrides (FormRequest types), prefer
    /// [`Request::body_bytes_with_cap`].
    ///
    /// `Content-Length` is parsed from headers and used for pre-rejection
    /// when present; otherwise the cap is enforced progressively while
    /// reading.
    pub async fn body_bytes(self) -> Result<(RequestParts, Bytes), FrameworkError> {
        self.body_bytes_with_cap(global_max_request_body_bytes())
            .await
    }

    /// Consume the request and collect the body as bytes, capped at
    /// `max_bytes`.
    ///
    /// Use this from `FormRequest::extract` to honor per-struct overrides
    /// (`FormRequest::max_body_bytes`). For the default global cap, prefer
    /// the simpler [`Request::body_bytes`].
    ///
    /// `Content-Length` is parsed from headers and used for pre-rejection
    /// when present (HTTP 413 with no body bytes read); otherwise the cap
    /// is enforced progressively while reading.
    pub async fn body_bytes_with_cap(
        self,
        max_bytes: usize,
    ) -> Result<(RequestParts, Bytes), FrameworkError> {
        let content_type = self
            .parts
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let content_length = self
            .parts
            .headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        let params = self.params;

        let bytes = match self.body {
            BodyState::Streaming(incoming) => {
                collect_body_with_cap(incoming, content_length, max_bytes).await?
            }
            // Middleware buffered the body upstream (typically the CSRF
            // middleware). Return the cached bytes without touching the
            // stream (which is gone) so downstream extractors still see
            // the original payload.
            BodyState::Buffered(b) => b,
            BodyState::Consumed => {
                return Err(FrameworkError::internal(
                    "Request body has already been consumed and was not buffered. \
                     This is a framework bug — middleware that drains the body \
                     must call Request::buffer_body before passing the request \
                     downstream.",
                ));
            }
        };

        Ok((
            RequestParts {
                params,
                content_type,
            },
            bytes,
        ))
    }

    /// Buffer the request body so subsequent reads can use the cache.
    ///
    /// Used by middleware that needs to inspect the body (e.g. the CSRF
    /// middleware reading the `_token` form field) and still pass the
    /// request to downstream handlers. After this returns, subsequent
    /// `body_bytes` / `form` / `json` reads on the same `Request` return
    /// the cached bytes without re-reading the underlying hyper stream.
    ///
    /// `max_bytes` caps the buffered size. Use the global cap
    /// ([`global_max_request_body_bytes`]) for general-purpose
    /// buffering, or a smaller cap when the middleware knows the body
    /// shape (e.g. CSRF caps form bodies at 64 KiB).
    ///
    /// Calling this twice is a no-op on the second call (the body is
    /// already buffered).
    pub async fn buffer_body(mut self, max_bytes: usize) -> Result<Self, FrameworkError> {
        let content_length = self
            .parts
            .headers
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());

        let body = std::mem::replace(&mut self.body, BodyState::Consumed);
        let bytes = match body {
            BodyState::Streaming(incoming) => {
                collect_body_with_cap(incoming, content_length, max_bytes).await?
            }
            BodyState::Buffered(b) => b,
            BodyState::Consumed => {
                return Err(FrameworkError::internal(
                    "Request body cannot be buffered: it was already consumed",
                ));
            }
        };
        self.body = BodyState::Buffered(bytes);
        Ok(self)
    }

    /// Read the cached body bytes set by [`Request::buffer_body`].
    /// Returns `None` if the body hasn't been buffered yet (or has been
    /// consumed). Use this from middleware that has called
    /// `buffer_body` and wants to inspect the bytes without consuming
    /// the request.
    pub fn cached_body(&self) -> Option<&Bytes> {
        match &self.body {
            BodyState::Buffered(b) => Some(b),
            _ => None,
        }
    }

    /// Parse the request body as JSON
    ///
    /// Consumes the request since the body can only be read once.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[derive(Deserialize)]
    /// struct CreateUser { name: String, email: String }
    ///
    /// pub async fn store(req: Request) -> Response {
    ///     let data: CreateUser = req.json().await?;
    ///     // ...
    /// }
    /// ```
    pub async fn json<T: DeserializeOwned>(self) -> Result<T, FrameworkError> {
        let (_, bytes) = self.body_bytes().await?;
        parse_json(&bytes)
    }

    /// Parse the request body as form-urlencoded
    ///
    /// Consumes the request since the body can only be read once.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// #[derive(Deserialize)]
    /// struct LoginForm { username: String, password: String }
    ///
    /// pub async fn login(req: Request) -> Response {
    ///     let form: LoginForm = req.form().await?;
    ///     // ...
    /// }
    /// ```
    pub async fn form<T: DeserializeOwned>(self) -> Result<T, FrameworkError> {
        let (_, bytes) = self.body_bytes().await?;
        parse_form(&bytes)
    }

    /// Parse the request body based on Content-Type header
    ///
    /// - `application/json` -> JSON parsing
    /// - `application/x-www-form-urlencoded` -> Form parsing
    /// - Otherwise -> JSON parsing (default)
    ///
    /// Consumes the request since the body can only be read once.
    pub async fn input<T: DeserializeOwned>(self) -> Result<T, FrameworkError> {
        let (parts, bytes) = self.body_bytes().await?;

        match parts.content_type.as_deref() {
            Some(ct) if ct.starts_with("application/x-www-form-urlencoded") => parse_form(&bytes),
            _ => parse_json(&bytes),
        }
    }

    /// Consume the request and return its parts along with the body state.
    ///
    /// This is used internally by the handler macro for FormRequest
    /// extraction and by the multipart upload code. Callers must
    /// pattern-match on [`BodyState`] — the body may be a streaming
    /// hyper body, an already-buffered `Bytes`, or fully consumed.
    pub fn into_parts(self) -> (RequestParts, BodyState) {
        let content_type = self
            .parts
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let params = self.params;

        (
            RequestParts {
                params,
                content_type,
            },
            self.body,
        )
    }
}

/// Request parts after body has been separated
///
/// Contains metadata needed for body parsing without the body itself.
#[derive(Clone)]
pub struct RequestParts {
    pub params: HashMap<String, String>,
    pub content_type: Option<String>,
}
