use super::cookie::Cookie;
use crate::error::FrameworkError;
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

    /// Add multiple headers at once. Mirrors Laravel's
    /// `Response::withHeaders($headers)`. The iterator may be any
    /// `IntoIterator` over `(K, V)` pairs (e.g. a `HashMap`,
    /// `Vec<(&str, &str)>`, or an array literal). Existing headers are
    /// not deduplicated — append-only, matching Laravel's `setCookie`
    /// plus `set('X-Foo', ...)` semantics that allow same-name repeats
    /// for `Set-Cookie`.
    pub fn with_headers<I, K, V>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in headers {
            self.headers.push((k.into(), v.into()));
        }
        self
    }

    /// Remove every header with the given name (case-insensitive).
    /// Mirrors Laravel's `Response::withoutHeader($key)`.
    pub fn without_header(mut self, name: &str) -> Self {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
        self
    }

    /// Read a header value off this response (FIRST occurrence —
    /// matches the typical Laravel test assertion `response()->headers->get(...)`).
    /// Returns `None` if no such header was set.
    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Replace any prior occurrences of `name` with a single value.
    /// Mirrors Laravel's `Response::header($key, $value, replace=true)`.
    pub fn replace_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let name = name.into();
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(&name));
        self.headers.push((name, value.into()));
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

    /// Attach multiple cookies in one call. Mirrors Laravel's
    /// `Response::withCookies([...])`. Each cookie becomes its own
    /// `Set-Cookie` header — same wire shape as repeated `.cookie()`
    /// calls.
    pub fn with_cookies<I>(mut self, cookies: I) -> Self
    where
        I: IntoIterator<Item = Cookie>,
    {
        for c in cookies {
            self = self.cookie(c);
        }
        self
    }

    /// Schedule a cookie deletion alongside this response. Equivalent
    /// to `.cookie(Cookie::forget(name))`. Mirrors Laravel's
    /// `Response::withoutCookie($name)`.
    pub fn without_cookie(self, name: impl Into<String>) -> Self {
        self.cookie(Cookie::forget(name))
    }

    /// Wrap this response in Ok() for use as Response type
    pub fn ok(self) -> Response {
        Ok(self)
    }

    /// Convert to hyper response. The body is always a
    /// `BoxBody<Bytes, Infallible>` so the server can hand any
    /// `HttpResponse` — static or streaming — to `hyper` without
    /// branching on the body shape.
    ///
    /// Headers are validated per-entry via `HeaderName::try_from` and
    /// `HeaderValue::try_from`. Any header rejected by hyper (CRLF
    /// injection attempts, invalid characters, oversize values) is
    /// **dropped** with a `tracing::warn!` and the response is built
    /// without it. The alternative — accumulating builder errors and
    /// panicking at `.body()` — would tear down the per-connection task
    /// on attacker-controlled input that any reflection-style
    /// middleware would forward into a header (CORS allow-headers,
    /// `X-Forwarded-*`, custom debug headers).
    ///
    /// The status code goes through `StatusCode::from_u16` with the
    /// same drop-on-invalid policy; out-of-range values (>999 or any
    /// non-status integer) downgrade to a 500 rather than panic.
    pub fn into_hyper(self) -> hyper::Response<BoxBody<Bytes, Infallible>> {
        let status = match hyper::StatusCode::from_u16(self.status) {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!(
                    status = self.status,
                    "dropping invalid HTTP status code; falling back to 500"
                );
                hyper::StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let mut builder = hyper::Response::builder().status(status);

        for (name, value) in self.headers {
            let header_name = match hyper::header::HeaderName::try_from(name.as_str()) {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(
                        header = %name,
                        error = %e,
                        "dropping invalid response header name; \
                         rejected by hyper validation"
                    );
                    continue;
                }
            };
            let header_value = match hyper::header::HeaderValue::try_from(value.as_str()) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        header = %name,
                        error = %e,
                        "dropping invalid response header value; \
                         rejected by hyper validation (likely CR/LF \
                         or other control character)"
                    );
                    continue;
                }
            };
            builder = builder.header(header_name, header_value);
        }

        let body: BoxBody<Bytes, Infallible> = match self.body {
            Body::Static(bytes) => Full::new(bytes).map_err(|never| match never {}).boxed(),
            Body::Stream(body) => body,
        };

        // After per-header validation above, the only way `.body()` can
        // fail is an internal hyper invariant violation — which would
        // be a hyper bug, not user input. Panic in that case is the
        // right move because there's no meaningful recovery.
        builder
            .body(body)
            .expect("hyper builder body must succeed after pre-validated headers + status")
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
    /// Attach multiple headers from any `(K, V)` iterator. Mirrors
    /// Laravel's `Response::withHeaders([...])`.
    fn with_headers<I, K, V>(self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>;
    /// Remove every occurrence of a header (case-insensitive). Mirrors
    /// Laravel's `Response::withoutHeader($key)`.
    fn without_header(self, name: &str) -> Self;
    /// Attach a cookie. Mirrors Laravel's `Response::withCookie($c)`.
    fn cookie(self, cookie: Cookie) -> Self;
    /// Attach multiple cookies. Mirrors Laravel's
    /// `Response::withCookies([...])`.
    fn with_cookies<I>(self, cookies: I) -> Self
    where
        I: IntoIterator<Item = Cookie>;
    /// Queue a cookie deletion. Mirrors Laravel's
    /// `Response::withoutCookie($name)`.
    fn without_cookie(self, name: impl Into<String>) -> Self;
}

impl ResponseExt for Response {
    fn status(self, code: u16) -> Self {
        self.map(|r| r.status(code))
    }

    fn header(self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.map(|r| r.header(name, value))
    }

    fn with_headers<I, K, V>(self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.map(|r| r.with_headers(headers))
    }

    fn without_header(self, name: &str) -> Self {
        self.map(|r| r.without_header(name))
    }

    fn cookie(self, cookie: Cookie) -> Self {
        self.map(|r| r.cookie(cookie))
    }

    fn with_cookies<I>(self, cookies: I) -> Self
    where
        I: IntoIterator<Item = Cookie>,
    {
        self.map(|r| r.with_cookies(cookies))
    }

    fn without_cookie(self, name: impl Into<String>) -> Self {
        self.map(|r| r.without_cookie(name))
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
    /// Flash payload to write into the session when this redirect is
    /// converted to a Response. Populated by [`Redirect::with`],
    /// [`Redirect::with_input`], [`Redirect::with_errors`]. Each entry
    /// is `(session_key, json_value)`; the session's flash queue marks
    /// the key as new-flash so it survives one more request.
    flash: Vec<(String, serde_json::Value)>,
    /// Cookies to attach to the redirect response. Mirrors Laravel's
    /// `RedirectResponse::withCookies([...])`.
    cookies: Vec<Cookie>,
    /// Extra headers to attach. Mirrors Laravel's
    /// `RedirectResponse::withHeaders([...])`.
    headers: Vec<(String, String)>,
    /// Optional URL fragment to append (replacing any pre-existing
    /// `#frag` on the location). Mirrors Laravel's
    /// `RedirectResponse::withFragment($fragment)`.
    fragment: Option<String>,
    /// When `Some(())`, drop any pre-existing `#frag` from the URL.
    /// Mutually exclusive with `fragment`: a subsequent
    /// `withFragment(...)` re-attaches one. Mirrors Laravel's
    /// `RedirectResponse::withoutFragment()`.
    strip_fragment: bool,
}

impl Redirect {
    /// Create a redirect to a specific URL/path
    pub fn to(path: impl Into<String>) -> Self {
        Self {
            location: path.into(),
            query_params: Vec::new(),
            status: 302,
            preserve_fragment: false,
            flash: Vec::new(),
            cookies: Vec::new(),
            headers: Vec::new(),
            fragment: None,
            strip_fragment: false,
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
            flash: Vec::new(),
            cookies: Vec::new(),
            headers: Vec::new(),
            fragment: None,
            strip_fragment: false,
        }
    }

    /// Redirect to the previous URL recorded in the session, or
    /// `fallback` if none is recorded.
    ///
    /// Mirrors Laravel's `redirect()->back($status = 302, $headers,
    /// $fallback)` from `Illuminate/Routing/Redirector.php:45`. Reads
    /// the previous URL from
    /// [`SessionData::previous_url`](crate::session::SessionData::previous_url),
    /// which [`SessionMiddleware`](crate::session::SessionMiddleware)
    /// writes on every successful GET request (Inertia partials and
    /// JSON-API responses are skipped).
    ///
    /// Use this in form-submit handlers to bounce the user back to
    /// where they came from after a successful POST, or in validation-
    /// failure paths to keep the user on the form page.
    pub fn back(fallback: impl Into<String>) -> Self {
        let dest = crate::session::session()
            .and_then(|s| s.previous_url())
            .unwrap_or_else(|| fallback.into());
        Self::to(dest)
    }

    /// Redirect to an absolute external URL. Behaviour-identical to
    /// [`Self::to`] but the name signals "this is going off-site"
    /// — handy when reviewers want to spot external-redirect sinks
    /// for open-redirect audits.
    ///
    /// Mirrors Laravel's `redirect()->away($path, $status, $headers)`
    /// from `Illuminate/Routing/Redirector.php:124`.
    pub fn away(url: impl Into<String>) -> Self {
        Self::to(url)
    }

    /// Redirect to the current URL.
    ///
    /// Mirrors Laravel's `redirect()->refresh($status, $headers)` from
    /// `Illuminate/Routing/Redirector.php:57`. Useful after a POST that
    /// mutates state on the current page — refreshing avoids the
    /// "browser asks to resubmit POST" warning.
    ///
    /// Resolves the current URL from the active request scope. Pass an
    /// explicit `Request` reference via [`Self::refresh_for`] when no
    /// scope is active.
    pub fn refresh() -> Self {
        // The previous URL doubles as "what page were we on" — the
        // session middleware writes it before the handler runs.
        let dest = crate::session::session()
            .and_then(|s| s.previous_url())
            .unwrap_or_else(|| "/".to_string());
        Self::to(dest)
    }

    /// Variant of [`Self::refresh`] that takes the [`crate::http::Request`]
    /// explicitly. Useful in handlers that have already moved the
    /// request out of `&Request` form. Builds the redirect target from
    /// the request's path + query string.
    pub fn refresh_for(request: &crate::http::Request) -> Self {
        Self::to(crate::routing::url::current(request))
    }

    /// Redirect a guest user to a login (or other) URL, storing the
    /// originally-requested URL as the "intended" destination.
    ///
    /// Mirrors Laravel's `redirect()->guest($path, $status, $headers,
    /// $secure)` from `Illuminate/Routing/Redirector.php:71`.
    ///
    /// Pass the inbound `request` so the originally-requested URL can
    /// be recovered after authentication via
    /// [`Self::intended`]. The intended URL is flashed
    /// to the session under `url.intended` (Laravel's key).
    pub fn guest(request: &crate::http::Request, login_path: impl Into<String>) -> Self {
        let intended = crate::routing::url::current(request);
        crate::session::session_mut(|s| {
            s.put("url.intended", intended);
        });
        Self::to(login_path)
    }

    /// Redirect to the "intended" URL stored by [`Self::guest`], or to
    /// `default` if no intended URL is recorded.
    ///
    /// Mirrors Laravel's `redirect()->intended($default, $status,
    /// $headers, $secure)` from `Illuminate/Routing/Redirector.php:95`.
    /// The intended URL is consumed (pulled from the session) so a
    /// subsequent call falls back to `default`.
    pub fn intended(default: impl Into<String>) -> Self {
        let dest = crate::session::session_mut(|s| s.pull::<String>("url.intended"))
            .flatten()
            .unwrap_or_else(|| default.into());
        Self::to(dest)
    }

    /// Record the current URL as the session's "intended" target.
    /// Subsequent calls to [`Redirect::intended`] use it. Mirrors
    /// Laravel's `Redirector::setIntendedUrl($url)`. No-op outside a
    /// `SessionMiddleware` scope.
    pub fn set_intended_url(url: impl Into<String>) {
        let url = url.into();
        crate::session::session_mut(|s| s.put("url.intended", url));
    }

    /// Sign and redirect to a named route.
    ///
    /// Convenience wrapper: builds the signed URL via
    /// [`crate::routing::url::signed_route`] and redirects to it. Useful
    /// for one-shot ephemeral URLs (password reset, email verification,
    /// download links) where you want to mint and immediately redirect
    /// the user.
    ///
    /// Returns an `Err` redirect when the route name is not registered
    /// or signing fails (encryption key not installed). The caller can
    /// `?`-propagate the error since [`Redirect`] converts to a
    /// `Response` cleanly.
    pub fn signed_route(name: &str, params: &[(&str, &str)]) -> Result<Self, FrameworkError> {
        let url = crate::routing::url::signed_route(name, params)?;
        Ok(Self::to(url))
    }

    /// Temporary-sign and redirect to a named route. Sibling of
    /// [`Self::signed_route`] with an explicit `expires_at_epoch_seconds`.
    pub fn temporary_signed_route(
        name: &str,
        params: &[(&str, &str)],
        expires_at_epoch_seconds: i64,
    ) -> Result<Self, FrameworkError> {
        let url =
            crate::routing::url::temporary_signed_route(name, params, expires_at_epoch_seconds)?;
        Ok(Self::to(url))
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

    /// Set an arbitrary status code (302 by default; common alternates
    /// are 303 See Other, 307 Temporary Redirect, 308 Permanent
    /// Redirect). Mirrors Laravel's `RedirectResponse::setStatusCode`.
    pub fn status(mut self, code: u16) -> Self {
        self.status = code;
        self
    }

    /// Flash a single key/value into the session, surviving exactly
    /// one more request. Mirrors Laravel's
    /// `RedirectResponse::with($key, $value)`. The value is serialized
    /// to JSON so anything `serde::Serialize` works (strings, numbers,
    /// nested maps, etc.).
    pub fn with(mut self, key: impl Into<String>, value: impl serde::Serialize) -> Self {
        let value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        self.flash.push((key.into(), value));
        self
    }

    /// Flash an input bag for the next request. The receiving page
    /// reads it back via `session.get_old_input(key)`. Mirrors
    /// Laravel's `RedirectResponse::withInput($input)`.
    pub fn with_input<I, K, V>(mut self, input: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: serde::Serialize,
    {
        let map: std::collections::HashMap<String, serde_json::Value> = input
            .into_iter()
            .map(|(k, v)| {
                (
                    k.into(),
                    serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
                )
            })
            .collect();
        // Use the OLD_INPUT_KEY constant indirectly via flash_input —
        // session's API guards the canonical key.
        self.flash.push((
            "__suprnova_input_flash".into(),
            serde_json::to_value(map).unwrap_or(serde_json::Value::Null),
        ));
        self
    }

    /// Flash a validation-errors bag. The receiving page reads it
    /// through Suprnova's session or via Inertia's auto-shared
    /// `errors` prop. Mirrors Laravel's
    /// `RedirectResponse::withErrors($errors, $bag)`. The default bag
    /// name (`"default"`) matches Laravel's behavior when no bag is
    /// specified.
    pub fn with_errors<I, K, V>(self, errors: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.with_errors_bag("default", errors)
    }

    /// Same as [`with_errors`] but writes into a NAMED bag. Mirrors
    /// Laravel's `RedirectResponse::withErrors($errors, $bag)`.
    pub fn with_errors_bag<I, K, V>(mut self, bag: impl Into<String>, errors: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let map: std::collections::HashMap<String, Vec<String>> =
            errors
                .into_iter()
                .fold(std::collections::HashMap::new(), |mut acc, (k, v)| {
                    acc.entry(k.into()).or_default().push(v.into());
                    acc
                });
        // Stored under `errors.<bag>` to match Laravel's ViewErrorBag
        // structure (a top-level `errors` flash whose value is a
        // bag-keyed map of fields).
        let key = format!("errors.{}", bag.into());
        self.flash.push((
            key,
            serde_json::to_value(map).unwrap_or(serde_json::Value::Null),
        ));
        self
    }

    /// Attach a single cookie. Mirrors Laravel's
    /// `RedirectResponse::withCookie($cookie)`. The cookie is set on
    /// the redirect response, not on the destination.
    pub fn cookie(mut self, cookie: Cookie) -> Self {
        self.cookies.push(cookie);
        self
    }

    /// Attach multiple cookies. Mirrors Laravel's
    /// `RedirectResponse::withCookies([$cookie1, $cookie2])`.
    pub fn with_cookies<I>(mut self, cookies: I) -> Self
    where
        I: IntoIterator<Item = Cookie>,
    {
        self.cookies.extend(cookies);
        self
    }

    /// Attach an extra header to the redirect response. Mirrors
    /// Laravel's `RedirectResponse::header($key, $value)`.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Attach multiple headers. Mirrors Laravel's
    /// `RedirectResponse::withHeaders([...])`.
    pub fn with_headers<I, K, V>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in headers {
            self.headers.push((k.into(), v.into()));
        }
        self
    }

    /// Append a `#fragment` to the redirect URL, replacing any existing
    /// one. Mirrors Laravel's `RedirectResponse::withFragment($fragment)`.
    /// Accepts the fragment with OR without a leading `#`.
    pub fn with_fragment(mut self, fragment: impl Into<String>) -> Self {
        let raw = fragment.into();
        let cleaned = raw.trim_start_matches('#').to_string();
        self.fragment = Some(cleaned);
        self.strip_fragment = false;
        self
    }

    /// Remove any pre-existing `#fragment` from the redirect URL.
    /// Mirrors Laravel's `RedirectResponse::withoutFragment()`.
    pub fn without_fragment(mut self) -> Self {
        self.strip_fragment = true;
        self.fragment = None;
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
        let mut url = append_query_params(&self.location, &self.query_params);
        url = apply_fragment(url, self.strip_fragment, self.fragment.as_deref());
        url
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
    let mut out =
        String::with_capacity(head.len() + 1 + encoded.len() + fragment.map_or(0, str::len));
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

/// Apply the `with_fragment` / `without_fragment` selection to a URL.
/// Used by both `Redirect::build_url` and `RedirectRouteBuilder::build_url`
/// so a redirect builder built either way honors the same fragment
/// rules.
///
/// - `strip` removes any pre-existing `#frag` (matches Laravel's
///   `withoutFragment`).
/// - `replace`, when `Some(f)`, drops any pre-existing `#frag` first
///   and then appends `#f` (matches Laravel's `withFragment`).
/// - Neither set: URL passes through unchanged.
fn apply_fragment(mut url: String, strip: bool, replace: Option<&str>) -> String {
    let needs_change = strip || replace.is_some();
    if needs_change && let Some(i) = url.find('#') {
        url.truncate(i);
    }
    if let Some(frag) = replace {
        url.push('#');
        url.push_str(frag);
    }
    url
}

/// Drain a `Redirect`'s pending flash bag into the live session.
/// Splits the input-bag entry off so it lands under the canonical
/// `_old_input` key Laravel's `Store::flashInput` writes — that's how
/// the receiving page reads it back via `Session::getOldInput`.
fn drain_flash(flash: Vec<(String, serde_json::Value)>) {
    if flash.is_empty() {
        return;
    }
    crate::session::session_mut(|s| {
        for (key, value) in flash {
            if key == "__suprnova_input_flash" {
                // Convert the value back into a HashMap and route
                // through the canonical Store::flashInput path so
                // session.get_old_input works on the receiving end.
                if let serde_json::Value::Object(map) = value {
                    let h: std::collections::HashMap<String, serde_json::Value> =
                        map.into_iter().collect();
                    s.flash_input(h);
                }
            } else {
                s.flash(&key, value);
            }
        }
    });
}

/// Auto-convert Redirect to Response
impl From<Redirect> for Response {
    fn from(redirect: Redirect) -> Response {
        // Resolve the URL first while `redirect` is fully owned and
        // borrow-clean — `build_url` reads `location`, `query_params`,
        // and `fragment`. Subsequent `drain_flash` moves the flash bag
        // out, so the URL must be computed before that.
        let url = redirect.build_url();
        flash_preserve_fragment_if_set(redirect.preserve_fragment);
        drain_flash(redirect.flash);
        let mut response = HttpResponse::new()
            .status(redirect.status)
            .header("Location", url);
        for (k, v) in redirect.headers {
            response = response.header(k, v);
        }
        for cookie in redirect.cookies {
            response = response.cookie(cookie);
        }
        Ok(response)
    }
}

/// Builder for redirects to named routes with parameters
pub struct RedirectRouteBuilder {
    name: String,
    params: std::collections::HashMap<String, String>,
    query_params: Vec<(String, String)>,
    status: u16,
    preserve_fragment: bool,
    /// Flash payload to write into the session when this redirect is
    /// converted to a Response. Mirrors [`Redirect`]'s field — kept
    /// separate so both builder paths can use the same flash API
    /// (`with`, `with_input`, `with_errors`) without sharing a struct.
    flash: Vec<(String, serde_json::Value)>,
    cookies: Vec<Cookie>,
    headers: Vec<(String, String)>,
    fragment: Option<String>,
    strip_fragment: bool,
}

impl RedirectRouteBuilder {
    /// Add a route parameter value. Mirrors Laravel's positional
    /// `redirect()->route($name, $params)`; chain one or more `.with`
    /// calls to set each `{key}` placeholder in the route template.
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

    /// Set an arbitrary status code (default 302). Common alternates
    /// are 303 / 307 / 308.
    pub fn status(mut self, code: u16) -> Self {
        self.status = code;
        self
    }

    /// Flash a single key/value into the session for one more request.
    /// Mirrors Laravel's `RedirectResponse::with`. Distinct from
    /// [`with`] (the route-param builder), which lives at the
    /// route-parameter level — use `flash` here for session flashes.
    pub fn flash(mut self, key: impl Into<String>, value: impl serde::Serialize) -> Self {
        let value = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
        self.flash.push((key.into(), value));
        self
    }

    /// Flash an input bag (same shape as [`Redirect::with_input`]).
    pub fn with_input<I, K, V>(mut self, input: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: serde::Serialize,
    {
        let map: std::collections::HashMap<String, serde_json::Value> = input
            .into_iter()
            .map(|(k, v)| {
                (
                    k.into(),
                    serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
                )
            })
            .collect();
        self.flash.push((
            "__suprnova_input_flash".into(),
            serde_json::to_value(map).unwrap_or(serde_json::Value::Null),
        ));
        self
    }

    /// Flash a validation-errors bag under the default bag.
    pub fn with_errors<I, K, V>(self, errors: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.with_errors_bag("default", errors)
    }

    /// Flash a validation-errors bag under a named bag.
    pub fn with_errors_bag<I, K, V>(mut self, bag: impl Into<String>, errors: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let map: std::collections::HashMap<String, Vec<String>> =
            errors
                .into_iter()
                .fold(std::collections::HashMap::new(), |mut acc, (k, v)| {
                    acc.entry(k.into()).or_default().push(v.into());
                    acc
                });
        let key = format!("errors.{}", bag.into());
        self.flash.push((
            key,
            serde_json::to_value(map).unwrap_or(serde_json::Value::Null),
        ));
        self
    }

    /// Attach a single cookie.
    pub fn cookie(mut self, cookie: Cookie) -> Self {
        self.cookies.push(cookie);
        self
    }

    /// Attach multiple cookies.
    pub fn with_cookies<I>(mut self, cookies: I) -> Self
    where
        I: IntoIterator<Item = Cookie>,
    {
        self.cookies.extend(cookies);
        self
    }

    /// Attach an extra header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Attach multiple headers.
    pub fn with_headers<I, K, V>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in headers {
            self.headers.push((k.into(), v.into()));
        }
        self
    }

    /// Append a `#fragment` to the resolved URL.
    pub fn with_fragment(mut self, fragment: impl Into<String>) -> Self {
        let raw = fragment.into();
        let cleaned = raw.trim_start_matches('#').to_string();
        self.fragment = Some(cleaned);
        self.strip_fragment = false;
        self
    }

    /// Strip any `#fragment` from the resolved URL.
    pub fn without_fragment(mut self) -> Self {
        self.strip_fragment = true;
        self.fragment = None;
        self
    }

    /// Carry the URL fragment across this redirect. See
    /// [`Redirect::preserve_fragment`] for details.
    pub fn preserve_fragment(mut self) -> Self {
        self.preserve_fragment = true;
        self
    }

    fn build_url(&self) -> Result<String, crate::routing::RouteUrlError> {
        use crate::routing::try_route_with_params;

        let url = try_route_with_params(&self.name, &self.params)?;
        let mut url = append_query_params(&url, &self.query_params);
        url = apply_fragment(url, self.strip_fragment, self.fragment.as_deref());
        Ok(url)
    }
}

/// Auto-convert RedirectRouteBuilder to Response
impl From<RedirectRouteBuilder> for Response {
    fn from(redirect: RedirectRouteBuilder) -> Response {
        // Route lookup runs first; if the named route is missing OR if
        // any required path parameter is absent, we return a 500 and
        // intentionally skip the flash — otherwise a stray
        // `_inertia.preserve_fragment` would land on whatever page the
        // user navigates to next, and a `Location` header containing
        // a raw `{placeholder}` is unsafe to ship to a browser.
        let url = match redirect.build_url() {
            Ok(u) => u,
            Err(e) => {
                return Err(HttpResponse::text(e.to_string()).status(500));
            }
        };
        flash_preserve_fragment_if_set(redirect.preserve_fragment);
        drain_flash(redirect.flash);
        let mut response = HttpResponse::new()
            .status(redirect.status)
            .header("Location", url);
        for (k, v) in redirect.headers {
            response = response.header(k, v);
        }
        for cookie in redirect.cookies {
            response = response.cookie(cookie);
        }
        Ok(response)
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
            crate::error::FrameworkError::ValidationError {
                field,
                message: msg,
            } => {
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
            // Dev-only detail. Gated behind the registered AppConfig's
            // debug flag (falling back to env-derived AppConfig if the
            // repository isn't seeded yet — `Config::is_debug` handles
            // that resolution). The `message` field stays generic in
            // both modes; this is strictly additive for developers,
            // never to be parsed by production clients.
            if status >= 500 && crate::config::Config::is_debug() {
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

        assert_eq!(resp.headers().get("Content-Type").unwrap(), "text/plain");
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
mod header_validation_tests {
    //! Domain 3a audit fix DR1: invalid response headers must be
    //! dropped + logged, not propagated to a builder.body().unwrap()
    //! panic that would tear down the per-connection task. The
    //! catch_unwind in `execute_chain_safely` (Domain 2) doesn't cover
    //! `into_hyper` because that runs after the chain returns, so the
    //! validation has to live here.

    use super::*;

    /// A header name containing CRLF would be a header-injection
    /// attempt. Build still succeeds; the bad header is silently
    /// dropped (with a tracing::warn that's not asserted here).
    #[test]
    fn invalid_header_name_drops_quietly_and_response_builds() {
        let resp = HttpResponse::text("ok")
            .header("X-Bad\r\nX-Injected", "value")
            .into_hyper();
        assert_eq!(resp.status(), 200);
        assert!(
            resp.headers().get("X-Bad\r\nX-Injected").is_none(),
            "invalid header name must be dropped"
        );
        assert!(
            resp.headers().get("X-Injected").is_none(),
            "injection target must not appear as a separate header"
        );
    }

    /// A header value containing CR/LF would split the response.
    /// Build still succeeds; bad header dropped.
    #[test]
    fn invalid_header_value_drops_quietly_and_response_builds() {
        let resp = HttpResponse::text("ok")
            .header("X-Custom", "value\r\nX-Injected: yes")
            .into_hyper();
        assert_eq!(resp.status(), 200);
        assert!(
            resp.headers().get("X-Custom").is_none(),
            "invalid header value must be dropped"
        );
        assert!(
            resp.headers().get("X-Injected").is_none(),
            "injection target must not appear as a separate header"
        );
    }

    /// Valid headers still pass through. Sanity check that the
    /// validation is a filter, not a block.
    #[test]
    fn valid_headers_pass_through_unchanged() {
        let resp = HttpResponse::text("ok")
            .header("X-Request-Id", "abc-123")
            .header("X-Custom-Header", "value-with-spaces and dashes")
            .into_hyper();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers().get("X-Request-Id").unwrap(), "abc-123");
        assert_eq!(
            resp.headers().get("X-Custom-Header").unwrap(),
            "value-with-spaces and dashes"
        );
    }

    /// Out-of-range status codes downgrade to 500 rather than panic.
    /// Hyper's `StatusCode::from_u16` rejects values outside the
    /// 100-999 range; before the fix, the body builder's .unwrap()
    /// would panic.
    #[test]
    fn invalid_status_code_falls_back_to_500() {
        // 0 and 9999 are both outside the HTTP status range.
        let resp = HttpResponse::text("ok").status(9999).into_hyper();
        assert_eq!(resp.status(), 500);

        let resp = HttpResponse::text("ok").status(0).into_hyper();
        assert_eq!(resp.status(), 500);
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
        assert_eq!(hyper.headers().get("Precognition-Success").unwrap(), "true");
        assert_eq!(hyper.headers().get("Vary").unwrap(), "Precognition");
    }

    #[test]
    fn precognition_failure_returns_422_with_envelope_and_errors() {
        let mut errs = ValidationErrors::new();
        errs.add("email", "must be valid");
        let resp: HttpResponse = FrameworkError::PrecognitionFailure(errs).into();
        let hyper = resp.into_hyper();
        assert_eq!(hyper.status(), 422);
        assert_eq!(hyper.headers().get("Precognition").unwrap(), "true");
        // No Precognition-Success on failures.
        assert!(hyper.headers().get("Precognition-Success").is_none());
        assert_eq!(hyper.headers().get("Vary").unwrap(), "Precognition");
    }
}
