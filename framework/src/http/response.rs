use super::cookie::Cookie;
use bytes::Bytes;
use futures::Stream;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use std::convert::Infallible;

/// The body of an [`HttpResponse`].
///
/// Static bodies are the common case — fully buffered `Bytes` produced
/// by `HttpResponse::text` / `json` / `html`. Streaming bodies back
/// SSE, chunked downloads, and any other long-lived response surface
/// (added in Task 16/17 of the observability foundation work). Both
/// branches collapse to a single `BoxBody<Bytes, Infallible>` when
/// converted to a hyper response, so callers downstream (the server,
/// middleware) see a uniform body type.
pub enum Body {
    /// Fully buffered body.
    Static(Bytes),
    /// Streaming body. The stream yields raw `Bytes` chunks that are
    /// wrapped into `http_body::Frame::data(...)` at send time. We use
    /// `Infallible` for the error type because every chunk producer in
    /// the framework is responsible for turning its own errors into a
    /// terminal SSE/stream message before the stream ends — there is
    /// no place to surface a transport-level error to the client mid-
    /// response, so the body must be infallible.
    Stream(BoxBody<Bytes, Infallible>),
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Body::Static(b) => f.debug_tuple("Static").field(b).finish(),
            Body::Stream(_) => f.debug_tuple("Stream").field(&"<stream>").finish(),
        }
    }
}

/// HTTP Response builder providing Laravel-like response creation
pub struct HttpResponse {
    status: u16,
    body: Body,
    headers: Vec<(String, String)>,
}

/// Response type alias - allows using `?` operator for early returns
pub type Response = Result<HttpResponse, HttpResponse>;

impl HttpResponse {
    pub fn new() -> Self {
        Self {
            status: 200,
            body: Body::Static(Bytes::new()),
            headers: Vec::new(),
        }
    }

    /// Create a response with a string body
    pub fn text(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: Body::Static(Bytes::from(body.into())),
            headers: vec![("Content-Type".to_string(), "text/plain".to_string())],
        }
    }

    /// Create a JSON response from a serde_json::Value
    pub fn json(body: serde_json::Value) -> Self {
        Self {
            status: 200,
            body: Body::Static(Bytes::from(body.to_string())),
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
        }
    }

    /// Create an HTML response. Sets `Content-Type: text/html; charset=utf-8`.
    pub fn html(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: Body::Static(Bytes::from(body.into())),
            headers: vec![(
                "Content-Type".to_string(),
                "text/html; charset=utf-8".to_string(),
            )],
        }
    }

    /// Build a streaming response from a `Stream` of `Bytes` chunks.
    ///
    /// The stream is wrapped into an `http_body::Frame` per chunk and
    /// boxed into a uniform `BoxBody<Bytes, Infallible>` so the rest
    /// of the framework treats streaming and static responses the same
    /// way. Used by [`HttpResponse::sse`] and any future chunked
    /// response surface.
    ///
    /// The stream's error type is `Infallible` by design — see
    /// [`Body::Stream`] for rationale.
    ///
    /// `Sync` is required because `BoxBody` is a shared trait object;
    /// every tokio channel adapter (`ReceiverStream`,
    /// `BroadcastStream`) and `futures::stream::iter` already satisfy
    /// this, so callers don't normally have to do anything special.
    pub fn stream_bytes<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<Bytes, Infallible>> + Send + Sync + 'static,
    {
        use futures::StreamExt;
        let framed = stream.map(|chunk| chunk.map(hyper::body::Frame::data));
        let stream_body = StreamBody::new(framed);
        Self {
            status: 200,
            body: Body::Stream(BoxBody::new(stream_body)),
            headers: Vec::new(),
        }
    }

    /// Set the HTTP status code
    pub fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    /// Get the configured HTTP status code.
    pub fn status_code(&self) -> u16 {
        self.status
    }

    /// Add a header to the response
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Add a Set-Cookie header to the response
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use suprnova::{Cookie, HttpResponse};
    ///
    /// let response = HttpResponse::text("OK")
    ///     .cookie(Cookie::new("session", "abc123"))
    ///     .cookie(Cookie::new("user_id", "42"));
    /// ```
    pub fn cookie(self, cookie: Cookie) -> Self {
        self.header("Set-Cookie", cookie.to_header_value())
    }

    /// Wrap this response in Ok() for use as Response type
    pub fn ok(self) -> Response {
        Ok(self)
    }

    /// Convert to hyper response. The body is always a
    /// `BoxBody<Bytes, Infallible>` so the server can hand any
    /// `HttpResponse` — static or streaming — to `hyper` without
    /// branching on the body shape.
    pub fn into_hyper(self) -> hyper::Response<BoxBody<Bytes, Infallible>> {
        let mut builder = hyper::Response::builder().status(self.status);

        for (name, value) in self.headers {
            builder = builder.header(name, value);
        }

        let body: BoxBody<Bytes, Infallible> = match self.body {
            Body::Static(bytes) => Full::new(bytes).map_err(|never| match never {}).boxed(),
            Body::Stream(body) => body,
        };

        builder.body(body).unwrap()
    }
}

impl Default for HttpResponse {
    fn default() -> Self {
        Self::new()
    }
}

/// Extension trait for Response to enable method chaining on macros
pub trait ResponseExt {
    fn status(self, code: u16) -> Self;
    fn header(self, name: impl Into<String>, value: impl Into<String>) -> Self;
}

impl ResponseExt for Response {
    fn status(self, code: u16) -> Self {
        self.map(|r| r.status(code))
    }

    fn header(self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.map(|r| r.header(name, value))
    }
}

/// HTTP Redirect response builder
pub struct Redirect {
    location: String,
    query_params: Vec<(String, String)>,
    status: u16,
    /// When `true`, on conversion to `Response` we flash
    /// `_inertia.preserve_fragment = true` into the session so the
    /// destination's `InertiaResponse` emits `preserveFragment: true`
    /// in its page object. Maps to Laravel's
    /// `redirect(...)->preserveFragment()`.
    preserve_fragment: bool,
}

impl Redirect {
    /// Create a redirect to a specific URL/path
    pub fn to(path: impl Into<String>) -> Self {
        Self {
            location: path.into(),
            query_params: Vec::new(),
            status: 302,
            preserve_fragment: false,
        }
    }

    /// Create a redirect to a named route
    pub fn route(name: &str) -> RedirectRouteBuilder {
        RedirectRouteBuilder {
            name: name.to_string(),
            params: std::collections::HashMap::new(),
            query_params: Vec::new(),
            status: 302,
            preserve_fragment: false,
        }
    }



    /// Add a query parameter
    pub fn query(mut self, key: &str, value: impl Into<String>) -> Self {
        self.query_params.push((key.to_string(), value.into()));
        self
    }

    /// Set status to 301 (Moved Permanently)
    pub fn permanent(mut self) -> Self {
        self.status = 301;
        self
    }

    /// Carry the URL fragment from the originating request across this
    /// redirect to the destination. On conversion to a `Response`, this
    /// flashes `_inertia.preserve_fragment = true` into the session;
    /// the next Inertia response (which is the redirect destination)
    /// picks up the flag and emits `preserveFragment: true` in its
    /// page object, telling the client to keep the URL hash.
    ///
    /// Requires `SessionMiddleware` to be active (it normally is).
    /// Without a session scope, the flag is silently dropped.
    ///
    /// Maps to Laravel's `redirect(...)->preserveFragment()`.
    pub fn preserve_fragment(mut self) -> Self {
        self.preserve_fragment = true;
        self
    }

    fn build_url(&self) -> String {
        if self.query_params.is_empty() {
            self.location.clone()
        } else {
            let query = self
                .query_params
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join("&");
            format!("{}?{}", self.location, query)
        }
    }
}

/// Flash `_inertia.preserve_fragment = true` into the session when the
/// redirect's preserve-fragment flag is set. Shared between the
/// `From<Redirect>` and `From<RedirectRouteBuilder>` impls so they
/// can't drift on flash behavior. No-op outside a `SessionMiddleware`
/// scope (silently dropped — by design, for tests / partial setups).
fn flash_preserve_fragment_if_set(preserve: bool) {
    if preserve {
        crate::session::session_mut(|s| {
            s.flash("_inertia.preserve_fragment", true);
        });
    }
}

/// Auto-convert Redirect to Response
impl From<Redirect> for Response {
    fn from(redirect: Redirect) -> Response {
        flash_preserve_fragment_if_set(redirect.preserve_fragment);
        Ok(HttpResponse::new()
            .status(redirect.status)
            .header("Location", redirect.build_url()))
    }
}

/// Builder for redirects to named routes with parameters
pub struct RedirectRouteBuilder {
    name: String,
    params: std::collections::HashMap<String, String>,
    query_params: Vec<(String, String)>,
    status: u16,
    preserve_fragment: bool,
}

impl RedirectRouteBuilder {
    /// Add a route parameter value
    pub fn with(mut self, key: &str, value: impl Into<String>) -> Self {
        self.params.insert(key.to_string(), value.into());
        self
    }

    /// Add a query parameter
    pub fn query(mut self, key: &str, value: impl Into<String>) -> Self {
        self.query_params.push((key.to_string(), value.into()));
        self
    }

    /// Set status to 301 (Moved Permanently)
    pub fn permanent(mut self) -> Self {
        self.status = 301;
        self
    }

    /// Carry the URL fragment across this redirect. See
    /// [`Redirect::preserve_fragment`] for details.
    pub fn preserve_fragment(mut self) -> Self {
        self.preserve_fragment = true;
        self
    }

    fn build_url(&self) -> Option<String> {
        use crate::routing::route_with_params;

        let mut url = route_with_params(&self.name, &self.params)?;
        if !self.query_params.is_empty() {
            let query = self
                .query_params
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join("&");
            url = format!("{}?{}", url, query);
        }
        Some(url)
    }
}

/// Auto-convert RedirectRouteBuilder to Response
impl From<RedirectRouteBuilder> for Response {
    fn from(redirect: RedirectRouteBuilder) -> Response {
        // Route lookup runs first; if the named route is missing, we
        // return a 500 and intentionally skip the flash — otherwise
        // a stray `_inertia.preserve_fragment` would land on whatever
        // page the user navigates to next.
        let url = redirect.build_url().ok_or_else(|| {
            HttpResponse::text(format!("Route '{}' not found", redirect.name)).status(500)
        })?;
        flash_preserve_fragment_if_set(redirect.preserve_fragment);
        Ok(HttpResponse::new()
            .status(redirect.status)
            .header("Location", url))
    }
}

/// Auto-convert FrameworkError to HttpResponse
///
/// This enables using the `?` operator in controller handlers to propagate
/// framework errors as appropriate HTTP responses.
impl From<crate::error::FrameworkError> for HttpResponse {
    fn from(err: crate::error::FrameworkError) -> HttpResponse {
        // Precognition gets early-exit treatment: success → 204 with
        // headers and no body; failure → 422 with errors body and the
        // Precognition envelope. Both responses carry `Vary: Precognition`
        // so caches don't confuse Precognition responses with regular
        // form-submission responses.
        match &err {
            crate::error::FrameworkError::PrecognitionSuccess => {
                return HttpResponse::new()
                    .status(204)
                    .header("Precognition", "true")
                    .header("Precognition-Success", "true")
                    .header("Vary", "Precognition");
            }
            crate::error::FrameworkError::PrecognitionFailure(errors) => {
                return HttpResponse::json(errors.to_json())
                    .status(422)
                    .header("Precognition", "true")
                    .header("Vary", "Precognition");
            }
            _ => {}
        }

        let status = err.status_code();
        let message = err.to_string();
        let request_id = crate::logging::current_request_id().map(|id| id.as_str().to_string());

        if status >= 500 {
            tracing::error!(
                status,
                error = %message,
                request_id = ?request_id,
                "framework error"
            );
            // Dispatch ErrorOccurred. Spawn so we don't block response
            // conversion on listener execution.
            let evt = crate::events::ErrorOccurred {
                error_message: message.clone(),
                status_code: status,
                request_id: request_id.clone(),
            };
            tokio::spawn(async move {
                let _ = crate::events::EventFacade::dispatch(evt).await;
            });
        } else if status >= 400 {
            tracing::warn!(
                status,
                error = %message,
                request_id = ?request_id,
                "client error"
            );
        }
        let body = match &err {
            crate::error::FrameworkError::ParamError { param_name } => {
                serde_json::json!({
                    "error": format!("Missing required parameter: {}", param_name)
                })
            }
            crate::error::FrameworkError::ValidationError { field, message } => {
                serde_json::json!({
                    "error": "Validation failed",
                    "field": field,
                    "message": message
                })
            }
            crate::error::FrameworkError::Validation(errors) => {
                // Laravel/Inertia-compatible validation error format
                errors.to_json()
            }
            crate::error::FrameworkError::Unauthorized => {
                serde_json::json!({
                    "message": "This action is unauthorized."
                })
            }
            _ => {
                serde_json::json!({
                    "error": err.to_string()
                })
            }
        };
        HttpResponse::json(body).status(status)
    }
}

/// Auto-convert AppError to HttpResponse
///
/// This enables using the `?` operator in controller handlers with AppError.
impl From<crate::error::AppError> for HttpResponse {
    fn from(err: crate::error::AppError) -> HttpResponse {
        // Convert AppError -> FrameworkError -> HttpResponse
        let framework_err: crate::error::FrameworkError = err.into();
        framework_err.into()
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use bytes::Bytes;
    use futures::stream;
    use http_body_util::BodyExt;
    use std::convert::Infallible;

    #[tokio::test]
    async fn streaming_response_emits_chunks_in_order() {
        let chunks: Vec<Result<Bytes, Infallible>> = vec![
            Ok(Bytes::from_static(b"chunk1\n")),
            Ok(Bytes::from_static(b"chunk2\n")),
        ];
        let resp = HttpResponse::stream_bytes(stream::iter(chunks))
            .header("Content-Type", "text/plain")
            .into_hyper();

        assert_eq!(
            resp.headers().get("Content-Type").unwrap(),
            "text/plain"
        );
        let collected = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"chunk1\nchunk2\n");
    }

    #[tokio::test]
    async fn static_body_round_trips_through_into_hyper() {
        let resp = HttpResponse::text("hello world").into_hyper();
        let collected = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], b"hello world");
    }
}

#[cfg(test)]
mod precognition_response_tests {
    use super::*;
    use crate::error::{FrameworkError, ValidationErrors};

    #[test]
    fn precognition_success_returns_204_with_envelope() {
        let resp: HttpResponse = FrameworkError::PrecognitionSuccess.into();
        let hyper = resp.into_hyper();
        assert_eq!(hyper.status(), 204);
        assert_eq!(hyper.headers().get("Precognition").unwrap(), "true");
        assert_eq!(
            hyper.headers().get("Precognition-Success").unwrap(),
            "true"
        );
        assert_eq!(hyper.headers().get("Vary").unwrap(), "Precognition");
    }

    #[test]
    fn precognition_failure_returns_422_with_envelope_and_errors() {
        let mut errs = ValidationErrors::new();
        errs.add("email", "must be valid");
        let resp: HttpResponse =
            FrameworkError::PrecognitionFailure(errs).into();
        let hyper = resp.into_hyper();
        assert_eq!(hyper.status(), 422);
        assert_eq!(hyper.headers().get("Precognition").unwrap(), "true");
        // No Precognition-Success on failures.
        assert!(hyper.headers().get("Precognition-Success").is_none());
        assert_eq!(hyper.headers().get("Vary").unwrap(), "Precognition");
    }
}
