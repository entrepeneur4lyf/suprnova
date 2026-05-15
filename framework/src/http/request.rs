use super::body::{collect_body, parse_form, parse_json};
use super::cookie::parse_cookies;
use super::ParamError;
use crate::error::FrameworkError;
use bytes::Bytes;
use serde::de::DeserializeOwned;
use std::collections::HashMap;

/// HTTP Request wrapper providing Laravel-like access to request data
pub struct Request {
    inner: hyper::Request<hyper::body::Incoming>,
    params: HashMap<String, String>,
}

impl Request {
    pub fn new(inner: hyper::Request<hyper::body::Incoming>) -> Self {
        Self {
            inner,
            params: HashMap::new(),
        }
    }

    pub fn with_params(mut self, params: HashMap<String, String>) -> Self {
        self.params = params;
        self
    }

    /// Get the request method
    pub fn method(&self) -> &hyper::Method {
        self.inner.method()
    }

    /// Get the request path
    pub fn path(&self) -> &str {
        self.inner.uri().path()
    }

    /// Returns the query string portion of the request URI (the part
    /// after `?`), or `None` when no query is present.
    pub fn query(&self) -> Option<&str> {
        self.inner.uri().query()
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

    /// Get the inner hyper request
    pub fn inner(&self) -> &hyper::Request<hyper::body::Incoming> {
        &self.inner
    }

    /// Get a header value by name
    pub fn header(&self, name: &str) -> Option<&str> {
        self.inner.headers().get(name).and_then(|v| v.to_str().ok())
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

    /// Consume the request and collect the body as bytes
    pub async fn body_bytes(self) -> Result<(RequestParts, Bytes), FrameworkError> {
        let content_type = self
            .inner
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let params = self.params;
        let bytes = collect_body(self.inner.into_body()).await?;

        Ok((
            RequestParts {
                params,
                content_type,
            },
            bytes,
        ))
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

    /// Consume the request and return its parts along with the inner hyper request body
    ///
    /// This is used internally by the handler macro for FormRequest extraction.
    pub fn into_parts(self) -> (RequestParts, hyper::body::Incoming) {
        let content_type = self
            .inner
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let params = self.params;
        let body = self.inner.into_body();

        (
            RequestParts {
                params,
                content_type,
            },
            body,
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
