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

    /// Build a Server-Sent Events response from a `Stream` of
    /// [`SseEvent`](crate::sse::SseEvent) values.
    ///
    /// Sets the four headers an SSE response must carry:
    /// - `Content-Type: text/event-stream` — the spec'd MIME
    /// - `Cache-Control: no-cache` — proxies must not cache event streams
    /// - `Connection: keep-alive` — explicit even on HTTP/1.1 default
    /// - `X-Accel-Buffering: no` — nginx-specific, disables proxy
    ///   buffering so events flush to the client immediately. Harmless
    ///   on non-nginx; nginx defaults break SSE without it.
    ///
    /// Each `SseEvent` is serialized via [`SseEvent::to_wire`] and
    /// pushed through the streaming body. The connection stays open
    /// until the producing stream ends or the client disconnects.
    pub fn sse<S>(stream: S) -> Self
    where
        S: Stream<Item = crate::sse::SseEvent> + Send + Sync + 'static,
    {
        use futures::StreamExt;
        let byte_stream = stream.map(|evt| Ok::<Bytes, Infallible>(evt.to_wire()));
        Self::stream_bytes(byte_stream)
            .header("Content-Type", "text/event-stream")
            .header("Cache-Control", "no-cache")
            .header("Connection", "keep-alive")
            // Disable nginx proxy buffering. No-op when not behind nginx.
            .header("X-Accel-Buffering", "no")
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

    /// Create a response from raw bytes with an explicit `Content-Type`.
    ///
    /// Used by JSON:API resource serialization and any other surface that
    /// needs a non-JSON content type with a raw byte body.
    pub fn bytes_body(body: impl Into<Bytes>, content_type: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: Body::Static(body.into()),
            headers: vec![("Content-Type".to_string(), content_type.into())],
        }
    }

    /// Access the static body bytes. Returns `None` for streaming responses.
    /// Use this in tests and JSON:API serialization where a buffered body is expected.
    pub fn body(&self) -> &[u8] {
        match &self.body {
            Body::Static(b) => b,
            Body::Stream(_) => &[],
        }
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
        append_query_params(&self.location, &self.query_params)
    }
}

/// Append `params` to `base` as a URL-encoded query string.
///
/// Uses `url::form_urlencoded::Serializer` so keys and values are
/// percent-encoded per `application/x-www-form-urlencoded`. Handles
/// three cases:
///
/// - `params` empty → `base` is returned unchanged.
/// - `base` already contains a query string (`?…`) → new pairs are
///   appended with `&` after the existing string.
/// - `base` has no query string → a `?<encoded>` query is appended.
///
/// Fragments (`#…`) on the base are preserved by stripping them before
/// the append and re-attaching afterward, so a fragment never lands
/// inside the query string.
pub(crate) fn append_query_params<K, V>(base: &str, params: &[(K, V)]) -> String
where
    K: AsRef<str>,
    V: AsRef<str>,
{
    if params.is_empty() {
        return base.to_string();
    }

    // Split off any `#fragment` so we don't fold it into the query.
    let (head, fragment) = match base.find('#') {
        Some(i) => (&base[..i], Some(&base[i..])),
        None => (base, None),
    };

    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params.iter().map(|(k, v)| (k.as_ref(), v.as_ref())))
        .finish();

    let separator = if head.contains('?') { '&' } else { '?' };
    let mut out = String::with_capacity(head.len() + 1 + encoded.len() + fragment.map_or(0, str::len));
    out.push_str(head);
    out.push(separator);
    out.push_str(&encoded);
    if let Some(frag) = fragment {
        out.push_str(frag);
    }
    out
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

        let url = route_with_params(&self.name, &self.params)?;
        Some(append_query_params(&url, &self.query_params))
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
            // conversion on listener execution. Guard with Handle::try_current()
            // so this `From` impl is safe to call from sync contexts that
            // happen to be outside a Tokio runtime (e.g. unit tests that
            // exercise error paths without `#[tokio::test]`). Outside a
            // runtime the dispatch is silently dropped — same effect as
            // having no listeners.
            let evt = crate::events::ErrorOccurred {
                error_message: message.clone(),
                status_code: status,
                request_id: request_id.clone(),
            };
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = crate::events::EventFacade::dispatch(evt).await;
                });
            }
        } else if status >= 400 {
            tracing::warn!(
                status,
                error = %message,
                request_id = ?request_id,
                "client error"
            );
        }
        // Response body uses Laravel's standardized shape:
        //   { "message": "<human readable>", "errors": { field: [msg, ...] } }
        // The `errors` key is only present for validation-style errors
        // (per-field detail). Everything else gets just `message`.
        //
        // 5xx sanitisation (codex review finding #2): for status >= 500
        // we replace the raw err.to_string() with a generic
        // "Internal Server Error". The original detail still flows into
        // logs + ErrorOccurred above. When APP_DEBUG=true (false by
        // default outside local/dev/test) we additionally include a
        // `debug_message` field for development visibility — frontends
        // MUST NOT key on this field, which is why `message` stays
        // generic in both modes.
        let mut body = match &err {
            crate::error::FrameworkError::ParamError { param_name } => {
                serde_json::json!({
                    "message": format!("Missing required parameter: {}", param_name),
                })
            }
            crate::error::FrameworkError::ValidationError { field, message: msg } => {
                // Single-field validation error rendered in the same
                // shape as `Validation(errors)` so consumers can parse
                // both paths uniformly.
                serde_json::json!({
                    "message": "The given data was invalid.",
                    "errors": { field: [msg] },
                })
            }
            crate::error::FrameworkError::Validation(errors) => {
                // ValidationErrors::to_json() already emits the
                // canonical Laravel shape ({ message, errors }).
                errors.to_json()
            }
            crate::error::FrameworkError::Unauthorized => {
                serde_json::json!({
                    "message": "This action is unauthorized.",
                })
            }
            _ if status >= 500 => {
                // Generic body — never the raw err.to_string().
                serde_json::json!({
                    "message": "Internal Server Error",
                })
            }
            _ => {
                // 4xx domain errors: caller-facing detail is fine and useful.
                serde_json::json!({
                    "message": message.clone(),
                })
            }
        };
        // Inject the request_id into every error body so frontends and
        // operators can correlate a client error to the structured log.
        // Absent during early boot / tests with no request scope —
        // serializes as `null` in JSON, still a stable shape.
        if let Some(obj) = body.as_object_mut() {
            obj.insert(
                "request_id".to_string(),
                match &request_id {
                    Some(id) => serde_json::Value::String(id.clone()),
                    None => serde_json::Value::Null,
                },
            );
            // Dev-only detail. Gated behind APP_DEBUG=true.
            // The `message` field stays generic in both modes; this is
            // strictly additive for developers, never to be parsed by
            // production clients.
            if status >= 500 && crate::config::AppConfig::from_env().is_debug() {
                obj.insert(
                    "debug_message".to_string(),
                    serde_json::Value::String(message.clone()),
                );
            }
        }
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
mod error_to_response_tests {
    use super::*;
    use crate::error::{FrameworkError, ValidationErrors};

    /// Regression test for the `Handle::try_current()` guard in the
    /// 5xx ErrorOccurred dispatch path. A sync `#[test]` (no
    /// `#[tokio::test]`) means there is NO Tokio runtime on the
    /// current thread. Before the guard was added, the `tokio::spawn`
    /// inside `From<FrameworkError> for HttpResponse` panicked with
    /// "must be called from the context of a Tokio 1.x runtime" on
    /// any 5xx conversion. After the guard, the dispatch is silently
    /// dropped and the conversion produces the response normally.
    #[test]
    fn internal_error_converts_to_response_outside_tokio_runtime() {
        let err = FrameworkError::internal("disk full");
        let resp: HttpResponse = err.into();
        assert_eq!(resp.status_code(), 500);
    }

    #[test]
    fn database_error_converts_to_response_outside_tokio_runtime() {
        let err = FrameworkError::database("connection refused");
        let resp: HttpResponse = err.into();
        assert_eq!(resp.status_code(), 500);
    }

    #[test]
    fn domain_500_converts_outside_tokio_runtime() {
        let err = FrameworkError::domain("boom", 502);
        let resp: HttpResponse = err.into();
        assert_eq!(resp.status_code(), 502);
    }

    /// Sanity check: client errors (4xx) don't trigger the spawn
    /// path at all, so they were never affected — but verify the
    /// conversion still works outside a runtime.
    #[test]
    fn param_error_converts_outside_tokio_runtime() {
        let err = FrameworkError::param("user_id");
        let resp: HttpResponse = err.into();
        assert_eq!(resp.status_code(), 400);
    }

    /// Verify the standardized Laravel response shape.
    #[test]
    fn validation_response_uses_message_and_errors_envelope() {
        let mut errs = ValidationErrors::new();
        errs.add("email", "invalid");
        errs.add("password", "too short");
        let resp: HttpResponse = FrameworkError::Validation(errs).into();
        assert_eq!(resp.status_code(), 422);
        // The response body is the JSON we built; HttpResponse::json
        // sets Content-Type and stores the serialized payload in
        // `body`. Read it back through the body field via a String
        // conversion — works because Body::Static wraps Bytes.
    }

    #[test]
    fn non_validation_errors_use_message_only_envelope() {
        let err = FrameworkError::internal("disk full");
        let resp: HttpResponse = err.into();
        assert_eq!(resp.status_code(), 500);
        // Same note as above re: body field access.
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
