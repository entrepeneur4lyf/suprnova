use super::ParamError;
use super::body::{collect_body_with_cap, global_max_request_body_bytes, parse_form, parse_json};
use super::cookie::parse_cookies;
use super::trusted_proxies::TrustedProxiesConfig;
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
    /// The matched route pattern (e.g. `/users/{id}`), if the router
    /// dispatched this request through a named or pattern-based match.
    /// Threaded in by the server after `Router::match_route` succeeds so
    /// downstream code can ask which route handled the request via
    /// [`Request::route_pattern`] / [`Request::route_name`] /
    /// [`Request::route_is`].
    route_pattern: Option<String>,
    /// The peer's IP address. Set by the server from the accepted-TCP
    /// `SocketAddr` when available, and consulted by [`Request::ip`] as
    /// the trusted fallback when no proxy header is present.
    peer_addr: Option<std::net::IpAddr>,
    /// Allowlist of TCP peer addresses whose `X-Forwarded-*` /
    /// `X-Real-IP` headers may be honoured by the proxy-aware
    /// accessors ([`Request::ip`], [`Request::secure`], [`Request::host`],
    /// [`Request::http_host`], [`Request::port`], [`Request::ips`]).
    ///
    /// Defaults to an empty config — proxy headers ignored. The server
    /// installs the [`AppConfig`](crate::config::AppConfig)-derived
    /// allowlist via [`Request::with_trusted_proxies`] in
    /// `handle_request_with_peer`; in-process tests can install one
    /// directly without touching the global container.
    trusted_proxies: TrustedProxiesConfig,
}

impl Request {
    /// Wrap a hyper request, splitting off the streaming body. Used by
    /// the server's request pipeline; in-process tests construct via
    /// [`crate::testing`] helpers instead.
    pub fn new(inner: hyper::Request<hyper::body::Incoming>) -> Self {
        let (parts, body) = inner.into_parts();
        Self {
            parts,
            body: BodyState::Streaming(body),
            params: HashMap::new(),
            route_pattern: None,
            peer_addr: None,
            trusted_proxies: TrustedProxiesConfig::empty(),
        }
    }

    /// Attach route parameters extracted from the path (e.g. `{id}`).
    /// Builder method called by the router after a successful match.
    pub fn with_params(mut self, params: HashMap<String, String>) -> Self {
        self.params = params;
        self
    }

    /// Record the matched route pattern (e.g. `/users/{id}`) on the
    /// request. Called by the server after `Router::match_route`
    /// resolves a pattern; downstream accessors
    /// ([`Request::route_pattern`], [`Request::route_name`],
    /// [`Request::route_is`]) read it back.
    pub fn with_route_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.route_pattern = Some(pattern.into());
        self
    }

    /// Record the connecting peer's IP address on the request. Called
    /// by the server from the accepted-TCP `SocketAddr`. Used as the
    /// trusted fallback by [`Request::ip`] when no proxy header is
    /// configured / present.
    pub fn with_peer_addr(mut self, addr: std::net::IpAddr) -> Self {
        self.peer_addr = Some(addr);
        self
    }

    /// Install the trusted-proxies allowlist for the proxy-aware
    /// accessors ([`Request::ip`], [`Request::secure`], [`Request::host`],
    /// [`Request::http_host`], [`Request::port`], [`Request::ips`]).
    ///
    /// Called by [`crate::server::handle_request_with_peer`] with the
    /// [`AppConfig`](crate::config::AppConfig)-derived allowlist
    /// resolved at request-entry; in-process tests can build a
    /// [`TrustedProxiesConfig`] directly and pass it here to assert
    /// proxy-trust behaviour without touching the global container.
    pub fn with_trusted_proxies(mut self, cfg: TrustedProxiesConfig) -> Self {
        self.trusted_proxies = cfg;
        self
    }

    /// The trusted-proxies allowlist currently installed on this
    /// request. Returns the empty default when no allowlist was
    /// configured. Useful for middleware that needs to consult the
    /// gating policy without invoking the accessors directly.
    pub fn trusted_proxies(&self) -> &TrustedProxiesConfig {
        &self.trusted_proxies
    }

    /// Whether the connecting TCP peer is in the trusted-proxy
    /// allowlist. The gating predicate behind every proxy-aware
    /// accessor; documented here so middleware can reuse it without
    /// duplicating the resolution logic.
    pub fn peer_is_trusted_proxy(&self) -> bool {
        self.trusted_proxies.trusts(self.peer_addr)
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
    /// ```rust,no_run
    /// # use suprnova::http::Request;
    /// # fn ex(req: &Request) {
    /// let cookies = req.cookies();
    /// if let Some(session) = cookies.get("session") {
    ///     println!("Session: {}", session);
    /// }
    /// # }
    /// ```
    pub fn cookies(&self) -> HashMap<String, String> {
        self.header("Cookie").map(parse_cookies).unwrap_or_default()
    }

    /// Get a specific cookie value by name
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use suprnova::http::Request;
    /// # fn ex(req: &Request) {
    /// if let Some(session_id) = req.cookie("session") {
    ///     // Use session_id
    /// }
    /// # }
    /// ```
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.cookies().get(name).cloned()
    }

    /// Returns `true` when a header with `name` is present (any value).
    /// Mirrors Laravel's `Request::hasHeader($key)`. Case-insensitive,
    /// matching hyper's `HeaderMap` semantics.
    pub fn has_header(&self, name: &str) -> bool {
        self.parts.headers.contains_key(name)
    }

    /// Read the bearer token from the `Authorization` header. Mirrors
    /// Laravel's `Request::bearerToken()`. Returns the substring after
    /// the LAST `Bearer ` (case-insensitive), stripped to the comma
    /// boundary if any (e.g. `Bearer foo, x=y` → `foo`). Returns `None`
    /// when the header is missing or does not carry a `Bearer ` prefix.
    pub fn bearer_token(&self) -> Option<String> {
        let raw = self.header("Authorization")?;
        // Mirror Laravel's `strripos` lookup: last occurrence of `Bearer `,
        // case-insensitive.
        let lower = raw.to_ascii_lowercase();
        let needle = "bearer ";
        let pos = lower.rfind(needle)?;
        let after = &raw[pos + needle.len()..];
        // Cut at the first comma (token list boundary) and trim
        // surrounding whitespace.
        let tok = after.split(',').next().unwrap_or("").trim();
        if tok.is_empty() {
            None
        } else {
            Some(tok.to_string())
        }
    }

    /// Returns `true` when the HTTP method equals `method`
    /// (case-insensitive). Mirrors Laravel's `Request::isMethod($method)`.
    pub fn is_method(&self, method: &str) -> bool {
        self.parts.method.as_str().eq_ignore_ascii_case(method)
    }

    /// Convenience: returns `true` when this is an XHR / AJAX request
    /// — the standard `X-Requested-With: XMLHttpRequest` header set by
    /// every browser XHR library. Mirrors Laravel's `Request::ajax()` /
    /// `Symfony Request::isXmlHttpRequest()`.
    pub fn ajax(&self) -> bool {
        self.header("X-Requested-With")
            .map(|v| v.eq_ignore_ascii_case("XMLHttpRequest"))
            .unwrap_or(false)
    }

    /// Returns `true` when the `X-PJAX` header is set to a truthy
    /// value. Mirrors Laravel's `Request::pjax()`.
    pub fn pjax(&self) -> bool {
        match self.header("X-PJAX") {
            None => false,
            Some(v) => {
                let v = v.trim();
                !v.is_empty() && !v.eq_ignore_ascii_case("false") && v != "0"
            }
        }
    }

    /// Returns `true` when the request is a prefetch hint — covers the
    /// Mozilla legacy `X-Moz: prefetch` and the modern `Purpose` /
    /// `Sec-Purpose: prefetch` family. Mirrors Laravel's
    /// `Request::prefetch()`.
    pub fn prefetch(&self) -> bool {
        let matches = |raw: Option<&str>| {
            raw.map(|v| v.eq_ignore_ascii_case("prefetch"))
                .unwrap_or(false)
        };
        matches(self.header("X-Moz"))
            || matches(self.header("Purpose"))
            || matches(self.header("Sec-Purpose"))
    }

    /// Returns `true` when the request is being served over HTTPS.
    ///
    /// Resolution order:
    /// 1. URI scheme on the request line (set by hyper when TLS is
    ///    terminated in-process).
    /// 2. `X-Forwarded-Proto` (single-value or first comma-split
    ///    value, case-insensitive) — only honoured when the TCP peer
    ///    matches the [trusted-proxy allowlist](TrustedProxiesConfig).
    /// 3. `X-Forwarded-Ssl: on` — older proxies (nginx legacy default),
    ///    same trusted-proxy gating.
    ///
    /// Mirrors Laravel's `Request::secure()` /
    /// `Symfony Request::isSecure()`.
    ///
    /// # Security note
    ///
    /// Default behaviour ignores proxy headers — the TCP peer is
    /// untrusted until the operator opts in via
    /// `APP_TRUSTED_PROXIES` (or
    /// [`AppConfigBuilder::trusted_proxies`](crate::config::AppConfigBuilder::trusted_proxies)).
    /// Without that opt-in, a client behind a terminating TLS proxy
    /// will read as `secure() == false` here — the framework cannot
    /// distinguish a real proxy hop from a spoofed `X-Forwarded-Proto`
    /// without an allowlist.
    pub fn secure(&self) -> bool {
        if let Some(scheme) = self.parts.uri.scheme_str()
            && scheme.eq_ignore_ascii_case("https")
        {
            return true;
        }
        if !self.peer_is_trusted_proxy() {
            return false;
        }
        if let Some(proto) = self.header("X-Forwarded-Proto") {
            let first = proto.split(',').next().unwrap_or("").trim();
            if first.eq_ignore_ascii_case("https") {
                return true;
            }
        }
        if let Some(ssl) = self.header("X-Forwarded-Ssl")
            && ssl.trim().eq_ignore_ascii_case("on")
        {
            return true;
        }
        false
    }

    /// URI scheme. Returns `"https"` when [`Request::secure`] is true,
    /// else `"http"`. Mirrors Symfony's `getScheme()`.
    pub fn scheme(&self) -> &'static str {
        if self.secure() { "https" } else { "http" }
    }

    /// Get the connecting peer IP address.
    ///
    /// Resolution order:
    /// 1. `X-Forwarded-For` — first non-empty comma-split value (only
    ///    when the TCP peer is in the trusted-proxy allowlist).
    /// 2. `X-Real-IP` — single value (same trusted-proxy gating).
    /// 3. The TCP peer address recorded by the server
    ///    ([`Request::with_peer_addr`]) — the fail-safe fallback used
    ///    whenever the proxy headers are absent or the peer is not a
    ///    trusted proxy.
    ///
    /// Returns `None` only when the peer-addr accessor is absent
    /// (e.g. tests that construct a `Request` directly from
    /// `Request::new(...)` without threading the peer) AND the
    /// configured proxy headers cannot be honoured. Mirrors Laravel's
    /// `Request::ip()` / `Symfony Request::getClientIp()`.
    ///
    /// # Security note
    ///
    /// `X-Forwarded-For` and `X-Real-IP` are client-controlled headers
    /// — any inbound request can carry them. They are honoured only
    /// when the TCP peer matches an address listed in
    /// [`AppConfig::trusted_proxies`](crate::config::AppConfig::trusted_proxies)
    /// (configurable via `APP_TRUSTED_PROXIES`). With the default
    /// empty allowlist, this method always returns the TCP peer.
    pub fn ip(&self) -> Option<String> {
        if self.peer_is_trusted_proxy() {
            if let Some(xff) = self.header("X-Forwarded-For") {
                // Return the first hop that parses as an IP and emit its
                // normalised form. A trusted proxy can still append a garbage
                // token, and never surfacing an unvalidated string also stops a
                // client from rotating rate-limit buckets with junk XFF values.
                if let Some(ip) = xff
                    .split(',')
                    .filter_map(|p| p.trim().parse::<std::net::IpAddr>().ok())
                    .next()
                {
                    return Some(ip.to_string());
                }
            }
            if let Some(real) = self.header("X-Real-IP")
                && let Ok(ip) = real.trim().parse::<std::net::IpAddr>()
            {
                return Some(ip.to_string());
            }
        }
        self.peer_addr.map(|ip| ip.to_string())
    }

    /// Full client IP chain, parsed from `X-Forwarded-For` plus the
    /// recorded peer address. Order: leftmost (originating client) →
    /// rightmost (closest hop). Mirrors Laravel's `Request::ips()` /
    /// `Symfony Request::getClientIps()`.
    ///
    /// `X-Forwarded-For` / `X-Real-IP` contribute to the chain only
    /// when the TCP peer matches the trusted-proxy allowlist — see
    /// [`Request::ip`] for the security rationale. The peer address
    /// itself is always appended (it is the only authoritative hop).
    pub fn ips(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.peer_is_trusted_proxy() {
            if let Some(xff) = self.header("X-Forwarded-For") {
                for piece in xff.split(',') {
                    // Validate each hop as an IP and emit the normalised form;
                    // drop anything that doesn't parse so a spoofed header can't
                    // inject arbitrary strings (e.g. markup) into the chain a
                    // consumer might render or log.
                    if let Ok(ip) = piece.trim().parse::<std::net::IpAddr>() {
                        out.push(ip.to_string());
                    }
                }
            }
            if let Some(real) = self.header("X-Real-IP")
                && let Ok(ip) = real.trim().parse::<std::net::IpAddr>()
            {
                let s = ip.to_string();
                if !out.iter().any(|v| v == &s) {
                    out.push(s);
                }
            }
        }
        if let Some(peer) = self.peer_addr {
            let s = peer.to_string();
            if !out.iter().any(|v| v == &s) {
                out.push(s);
            }
        }
        out
    }

    /// Read the `User-Agent` header. Returns `None` when absent or
    /// non-UTF-8. Mirrors Laravel's `Request::userAgent()`.
    pub fn user_agent(&self) -> Option<&str> {
        self.header("User-Agent")
    }

    /// The host name (no port, no scheme). Resolution:
    /// `X-Forwarded-Host` first (only when the TCP peer is in the
    /// trusted-proxy allowlist), then `Host` header, then URI
    /// authority host. Mirrors Symfony's `getHost()`. See
    /// [`Request::ip`] for the trusted-proxy security rationale.
    pub fn host(&self) -> Option<String> {
        if self.peer_is_trusted_proxy()
            && let Some(fhost) = self.header("X-Forwarded-Host")
        {
            let first = fhost.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                return Some(strip_port(first).to_string());
            }
        }
        if let Some(h) = self.header("Host") {
            return Some(strip_port(h).to_string());
        }
        self.parts.uri.host().map(|s| s.to_string())
    }

    /// The HTTP host being requested — host plus port when the port is
    /// non-default for the scheme. Mirrors Symfony's `getHttpHost()`.
    pub fn http_host(&self) -> Option<String> {
        let host = self.host()?;
        let scheme = self.scheme();
        let port = self.port();
        let default_port = match scheme {
            "https" => 443,
            _ => 80,
        };
        if let Some(p) = port
            && p != default_port
        {
            Some(format!("{host}:{p}"))
        } else {
            Some(host)
        }
    }

    /// Scheme + host + port as a single string. Mirrors Symfony's
    /// `getSchemeAndHttpHost()`.
    pub fn scheme_and_http_host(&self) -> Option<String> {
        let host = self.http_host()?;
        Some(format!("{}://{host}", self.scheme()))
    }

    /// The port the client is connecting to. Resolution: explicit
    /// `:port` on the `X-Forwarded-Host` header → explicit
    /// `X-Forwarded-Port` → explicit `:port` on the `Host` header →
    /// URI authority port → `None` (caller treats as the scheme
    /// default).
    ///
    /// `X-Forwarded-Host` / `X-Forwarded-Port` are honoured only when
    /// the TCP peer is in the trusted-proxy allowlist — see
    /// [`Request::ip`] for the security rationale.
    pub fn port(&self) -> Option<u16> {
        let trusted = self.peer_is_trusted_proxy();
        let forwarded_host = if trusted {
            self.header("X-Forwarded-Host")
                .and_then(|v| v.split(',').next())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
        } else {
            None
        };
        let host_header = forwarded_host.or_else(|| self.header("Host"));
        if let Some(h) = host_header
            && let Some(p) = port_of(h)
        {
            return Some(p);
        }
        if trusted
            && let Some(port) = self.header("X-Forwarded-Port")
            && let Ok(p) = port.trim().parse::<u16>()
        {
            return Some(p);
        }
        self.parts.uri.port_u16()
    }

    /// Decoded path: the request path with percent escapes resolved.
    /// Mirrors Laravel's `Request::decodedPath()`.
    pub fn decoded_path(&self) -> String {
        percent_encoding::percent_decode_str(self.path())
            .decode_utf8_lossy()
            .into_owned()
    }

    /// Get the path segments (split on `/`, empty segments dropped).
    /// 1-based access via [`Request::segment`]. Mirrors Laravel's
    /// `Request::segments()`.
    pub fn segments(&self) -> Vec<String> {
        self.decoded_path()
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    }

    /// Get a path segment by **1-based** index, or `default` when out
    /// of range. Mirrors Laravel's `Request::segment($index, $default)`.
    pub fn segment(&self, index: usize, default: Option<&str>) -> Option<String> {
        if index == 0 {
            return default.map(|s| s.to_string());
        }
        let segments = self.segments();
        segments
            .get(index - 1)
            .cloned()
            .or_else(|| default.map(|s| s.to_string()))
    }

    /// Get the URL of the request (no query string, trailing `/`
    /// stripped). Mirrors Laravel's `Request::url()`.
    pub fn url(&self) -> String {
        let scheme_host = self.scheme_and_http_host().unwrap_or_default();
        let path = self.path();
        let stripped = if path.len() > 1 {
            path.trim_end_matches('/')
        } else {
            path
        };
        format!("{scheme_host}{stripped}")
    }

    /// Get the full URL (URL + `?` + query string when present).
    /// Mirrors Laravel's `Request::fullUrl()`.
    pub fn full_url(&self) -> String {
        match self.query() {
            Some(q) if !q.is_empty() => format!("{}?{}", self.url(), q),
            _ => self.url(),
        }
    }

    /// Build the full URL with extra/overridden query params merged in.
    /// Mirrors Laravel's `Request::fullUrlWithQuery($query)`.
    pub fn full_url_with_query<K, V>(&self, extra: &[(K, V)]) -> String
    where
        K: AsRef<str>,
        V: AsRef<str>,
    {
        super::response::append_query_params(&self.full_url(), extra)
    }

    /// Build the full URL with the given query keys removed. Mirrors
    /// Laravel's `Request::fullUrlWithoutQuery($keys)`.
    pub fn full_url_without_query<K: AsRef<str>>(&self, keys: &[K]) -> String {
        let url = self.url();
        let q = match self.query() {
            Some(q) if !q.is_empty() => q,
            _ => return url,
        };
        let drop: std::collections::HashSet<&str> = keys.iter().map(|k| k.as_ref()).collect();
        let pairs: Vec<(String, String)> = url::form_urlencoded::parse(q.as_bytes())
            .filter(|(k, _)| !drop.contains(k.as_ref()))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if pairs.is_empty() {
            return url;
        }
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .finish();
        format!("{url}?{encoded}")
    }

    /// Parse the query string into a fresh HashMap. Repeated keys are
    /// flattened to the last value (the standard `application/x-www-
    /// form-urlencoded` convention). For typed access, prefer
    /// [`Request::query_into`] with a `serde::Deserialize` target.
    /// Mirrors Laravel's `Request::query()` (untyped form).
    pub fn query_params(&self) -> HashMap<String, String> {
        let q = match self.query() {
            Some(q) if !q.is_empty() => q,
            _ => return HashMap::new(),
        };
        url::form_urlencoded::parse(q.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    /// Look up a single query string value by key. Returns `None` when
    /// the key is absent. Mirrors Laravel's
    /// `Request::query($key, $default)`.
    pub fn query_param(&self, key: &str) -> Option<String> {
        let q = self.query()?;
        for (k, v) in url::form_urlencoded::parse(q.as_bytes()) {
            if k == key {
                return Some(v.into_owned());
            }
        }
        None
    }

    /// Returns `true` when a query string key is present.
    pub fn has_query(&self, key: &str) -> bool {
        self.query_param(key).is_some()
    }

    /// Deserialize the query string into a typed struct.
    /// Mirrors Laravel's typed query access (commonly used through
    /// validated form requests in PHP, but Suprnova users reach for
    /// this directly when they only care about query params).
    pub fn query_into<T: DeserializeOwned>(&self) -> Result<T, FrameworkError> {
        let q = self.query().unwrap_or("");
        serde_urlencoded::from_str(q)
            .map_err(|e| FrameworkError::domain(format!("query parse: {e}"), 422))
    }

    /// Returns the matched route pattern (e.g. `/users/{id}`) when the
    /// router dispatched this request, or `None` for the fallback
    /// branch / direct test construction.
    pub fn route_pattern(&self) -> Option<&str> {
        self.route_pattern.as_deref()
    }

    /// Returns the matched route's registered NAME (the value from
    /// `.name("users.show")`) when one was set, or `None` for an
    /// unnamed route or an unmatched request. Mirrors Laravel's
    /// `Request::route()->getName()`.
    pub fn route_name(&self) -> Option<String> {
        let pattern = self.route_pattern.as_deref()?;
        crate::routing::route_name_for_pattern(pattern)
    }

    /// Determine if the route name matches a given pattern (literal,
    /// or `*` wildcard via `str::is`). Mirrors Laravel's
    /// `Request::routeIs(...)`.
    pub fn route_is(&self, patterns: &[&str]) -> bool {
        let name = match self.route_name() {
            Some(n) => n,
            None => return false,
        };
        patterns.iter().any(|p| glob_match(p, &name))
    }

    /// Determine if the current request path matches any of the given
    /// patterns. Each pattern may contain `*` wildcards. Mirrors
    /// Laravel's `Request::is(...)`.
    pub fn is(&self, patterns: &[&str]) -> bool {
        let path = self.decoded_path();
        let stripped = path.trim_start_matches('/');
        patterns
            .iter()
            .any(|p| glob_match(p.trim_start_matches('/'), stripped))
    }

    /// Determine if the current full URL matches any of the given
    /// patterns. Each pattern may contain `*` wildcards. Mirrors
    /// Laravel's `Request::fullUrlIs(...)`.
    pub fn full_url_is(&self, patterns: &[&str]) -> bool {
        let full = self.full_url();
        patterns.iter().any(|p| glob_match(p, &full))
    }

    /// Determine if the request body is sending JSON (Content-Type
    /// contains `/json` or `+json`). Mirrors Laravel's
    /// `Request::isJson()`.
    pub fn is_json(&self) -> bool {
        self.header("content-type")
            .map(|v| {
                let v = v.to_ascii_lowercase();
                v.contains("/json") || v.contains("+json")
            })
            .unwrap_or(false)
    }

    /// Determine if the current request expects a JSON response.
    /// Mirrors Laravel's `Request::expectsJson()`.
    pub fn expects_json(&self) -> bool {
        (self.ajax() && !self.pjax() && self.accepts_any_content_type()) || self.wants_json()
    }

    /// Determine if the current request prefers JSON. Mirrors
    /// Laravel's `Request::wantsJson()`.
    pub fn wants_json(&self) -> bool {
        let types = self.acceptable_content_types();
        match types.first() {
            None => false,
            Some(t) => {
                let lower = t.to_ascii_lowercase();
                lower.contains("/json") || lower.contains("+json")
            }
        }
    }

    /// Return the list of acceptable content types in priority order,
    /// derived from the `Accept` header (q-value sorted descending).
    /// Mirrors Laravel's `Request::getAcceptableContentTypes()`.
    pub fn acceptable_content_types(&self) -> Vec<String> {
        let raw = match self.header("Accept") {
            Some(v) => v,
            None => return Vec::new(),
        };
        parse_accept(raw)
    }

    /// Determine if the request accepts ANY of the given content
    /// types. Mirrors Laravel's `Request::accepts($contentTypes)`.
    pub fn accepts(&self, content_types: &[&str]) -> bool {
        let accepts = self.acceptable_content_types();
        if accepts.is_empty() {
            return true;
        }
        for accept in &accepts {
            let bare = accept.split(';').next().unwrap_or(accept).trim();
            if bare == "*/*" || bare == "*" {
                return true;
            }
            let accept_lc = bare.to_ascii_lowercase();
            for ty in content_types {
                let ty_lc = ty.to_ascii_lowercase();
                if Self::matches_type(&accept_lc, &ty_lc)
                    || accept_lc == format!("{}/*", ty_lc.split('/').next().unwrap_or(""))
                {
                    return true;
                }
            }
        }
        false
    }

    /// Pick the most suitable response content type from the offered
    /// list, based on the request's `Accept` header. Returns `None`
    /// when none match. Mirrors Laravel's `Request::prefers($types)`.
    pub fn prefers(&self, content_types: &[&str]) -> Option<String> {
        let accepts = self.acceptable_content_types();
        for accept in &accepts {
            let bare = accept.split(';').next().unwrap_or(accept).trim();
            if bare == "*/*" || bare == "*" {
                return content_types.first().map(|s| s.to_string());
            }
            let accept_lc = bare.to_ascii_lowercase();
            for ty in content_types {
                let ty_lc = ty.to_ascii_lowercase();
                if Self::matches_type(&ty_lc, &accept_lc)
                    || accept_lc == format!("{}/*", ty_lc.split('/').next().unwrap_or(""))
                {
                    return Some((*ty).to_string());
                }
            }
        }
        None
    }

    /// Returns `true` when the request accepts any content type
    /// (no Accept header, or `*/*` / `*` as the top preference).
    /// Mirrors Laravel's `Request::acceptsAnyContentType()`.
    pub fn accepts_any_content_type(&self) -> bool {
        let acceptable = self.acceptable_content_types();
        acceptable.is_empty()
            || matches!(
                acceptable.first().map(|s| s.as_str()),
                Some("*/*") | Some("*")
            )
    }

    /// Convenience: returns `true` when the request accepts JSON
    /// (`application/json`). Mirrors Laravel's `Request::acceptsJson()`.
    pub fn accepts_json(&self) -> bool {
        self.accepts(&["application/json"])
    }

    /// Convenience: returns `true` when the request accepts HTML
    /// (`text/html`). Mirrors Laravel's `Request::acceptsHtml()`.
    pub fn accepts_html(&self) -> bool {
        self.accepts(&["text/html"])
    }

    /// Compare two content types where the wildcard `+suffix` form is
    /// tolerated (e.g. `application/json` matches
    /// `application/foo+json`). Mirrors Laravel's
    /// `Request::matchesType()`.
    fn matches_type(actual: &str, ty: &str) -> bool {
        if actual == ty {
            return true;
        }
        let actual_split: Vec<&str> = actual.split('/').collect();
        if actual_split.len() == 2 {
            let prefix = actual_split[0];
            let suffix = actual_split[1];
            // e.g. actual = application/json, ty = application/foo+json
            // should match — checking ty matches pattern `<prefix>/.+\+<suffix>`
            if let Some((ty_prefix, ty_rest)) = ty.split_once('/')
                && ty_prefix == prefix
                && let Some((_, ty_suffix)) = ty_rest.rsplit_once('+')
                && ty_suffix == suffix
            {
                return true;
            }
        }
        false
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
        let content_length = super::body::parse_content_length(&self.parts.headers);

        let params = self.params;

        let bytes = match self.body {
            BodyState::Streaming(incoming) => {
                collect_body_with_cap(incoming, content_length, max_bytes).await?
            }
            // Middleware buffered the body upstream (typically the CSRF
            // middleware). Return the cached bytes without touching the
            // stream (which is gone) so downstream extractors still see
            // the original payload. The cap still applies: a FormRequest
            // overriding `max_body_bytes` below the buffering middleware's
            // limit (e.g. the CSRF 64 KiB ceiling) must still see the
            // tighter bound honored, matching the streaming arm's 413.
            BodyState::Buffered(b) => {
                if b.len() > max_bytes {
                    return Err(FrameworkError::domain(
                        format!("request body exceeds {max_bytes} bytes (cap)"),
                        413,
                    ));
                }
                b
            }
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
        let content_length = super::body::parse_content_length(&self.parts.headers);

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
    /// ```rust,no_run
    /// # use suprnova::{Request, Response, HttpResponse};
    /// # use serde::Deserialize;
    /// #[derive(Deserialize)]
    /// struct CreateUser { name: String, email: String }
    ///
    /// pub async fn store(req: Request) -> Response {
    ///     let data: CreateUser = req.json().await?;
    ///     // ...
    /// #   Ok(HttpResponse::text(data.name))
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
    /// ```rust,no_run
    /// # use suprnova::{Request, Response, HttpResponse};
    /// # use serde::Deserialize;
    /// #[derive(Deserialize)]
    /// struct LoginForm { username: String, password: String }
    ///
    /// pub async fn login(req: Request) -> Response {
    ///     let form: LoginForm = req.form().await?;
    ///     // ...
    /// #   Ok(HttpResponse::text(form.username))
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
    /// Route parameters extracted from the matched pattern (e.g. `{id}`).
    pub params: HashMap<String, String>,
    /// Value of the `Content-Type` header, if the request carried one.
    pub content_type: Option<String>,
}

/// Strip a `:port` suffix from a host string, handling IPv6 brackets
/// (`[::1]:8080` → `[::1]`). Mirrors the host-only resolution Laravel
/// gets from Symfony's `HeaderUtils::parseAuthority`.
fn strip_port(host: &str) -> &str {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[') {
        // IPv6: take through the closing `]`, drop anything after.
        if let Some(end) = rest.find(']') {
            return &host[..end + 2];
        }
        return host;
    }
    match host.rfind(':') {
        Some(i) => &host[..i],
        None => host,
    }
}

/// Parse a `:port` from a host string. None when absent or unparseable.
fn port_of(host: &str) -> Option<u16> {
    let host = host.trim();
    let suffix = if let Some(rest) = host.strip_prefix('[') {
        // IPv6 in brackets: parse after `]:`.
        let end = rest.find(']')?;
        let after = &rest[end + 1..];
        after.strip_prefix(':')?
    } else {
        // IPv4 or hostname: only one `:` means the suffix is a port;
        // anything else is an unbracketed IPv6 literal (which has
        // multiple `:` characters) and has no port.
        if host.matches(':').count() != 1 {
            return None;
        }
        let idx = host.rfind(':')?;
        let suffix = &host[idx + 1..];
        if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        suffix
    };
    suffix.parse().ok()
}

/// Parse an `Accept` header into a list of content types in priority
/// order. Bare media types (no `q=`) are kept in source order ahead of
/// any explicitly lower-q items. Mirrors Symfony's `AcceptHeader`
/// q-sort, simplified to the subset Laravel exposes.
///
/// `q` values are clamped to `[0.0, 1.0]` per RFC 7231 §5.3.1, so a
/// malformed weight (e.g. `q=5`) cannot outrank a legitimately
/// top-priority type and invert content negotiation.
fn parse_accept(raw: &str) -> Vec<String> {
    let mut entries: Vec<(usize, f32, String)> = Vec::new();
    for (idx, piece) in raw.split(',').enumerate() {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let mut q: f32 = 1.0;
        // Pull out any `q=` parameter; bare params (no =) pass through
        // into the type string as-is.
        for param in piece.split(';').skip(1) {
            let p = param.trim();
            if let Some(qv) = p.strip_prefix("q=") {
                q = qv.parse::<f32>().unwrap_or(1.0).clamp(0.0, 1.0);
            } else if let Some(qv) = p.strip_prefix("Q=") {
                q = qv.parse::<f32>().unwrap_or(1.0).clamp(0.0, 1.0);
            }
        }
        entries.push((idx, q, piece.to_string()));
    }
    // Sort by q descending, preserving source order as the tie-break.
    entries.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    entries.into_iter().map(|(_, _, s)| s).collect()
}

/// Simple `*`-wildcard pattern match used by [`Request::is`] and
/// [`Request::route_is`]. Mirrors Laravel's `Str::is($pattern, $value)`
/// — `*` matches any sequence (including empty). No `?` single-char or
/// regex semantics; Laravel doesn't either.
fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == value || pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == value;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut idx = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            // First segment must anchor the start unless it's empty
            // (pattern began with `*`).
            if !value[idx..].starts_with(part) {
                return false;
            }
            idx += part.len();
        } else if i == parts.len() - 1 {
            // Last segment must anchor the end.
            if !value[idx..].ends_with(part) {
                return false;
            }
        } else if part.is_empty() {
            // Consecutive `**` collapses — no-op.
            continue;
        } else {
            match value[idx..].find(part) {
                Some(off) => idx += off + part.len(),
                None => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod url_helper_tests {
    use super::*;

    #[test]
    fn strip_port_handles_ipv4_and_hostname() {
        assert_eq!(strip_port("example.com"), "example.com");
        assert_eq!(strip_port("example.com:8080"), "example.com");
        assert_eq!(strip_port("127.0.0.1:9000"), "127.0.0.1");
    }

    #[test]
    fn strip_port_handles_ipv6_brackets() {
        assert_eq!(strip_port("[::1]"), "[::1]");
        assert_eq!(strip_port("[::1]:8080"), "[::1]");
        assert_eq!(strip_port("[2001:db8::1]:443"), "[2001:db8::1]");
    }

    #[test]
    fn port_of_returns_expected() {
        assert_eq!(port_of("example.com"), None);
        assert_eq!(port_of("example.com:8080"), Some(8080));
        assert_eq!(port_of("[::1]:9090"), Some(9090));
        // Bare IPv6 without brackets must NOT be misread as port:
        assert_eq!(port_of("::1"), None);
    }

    #[test]
    fn parse_accept_sorts_by_q_descending() {
        let parsed = parse_accept("text/html;q=0.5, application/json;q=0.9, */*;q=0.1");
        assert_eq!(
            parsed,
            vec![
                "application/json;q=0.9".to_string(),
                "text/html;q=0.5".to_string(),
                "*/*;q=0.1".to_string(),
            ]
        );
    }

    #[test]
    fn parse_accept_preserves_source_order_for_ties() {
        let parsed = parse_accept("text/html, application/xhtml+xml, application/json");
        assert_eq!(
            parsed,
            vec![
                "text/html".to_string(),
                "application/xhtml+xml".to_string(),
                "application/json".to_string(),
            ]
        );
    }

    /// A malformed `q` weight above the RFC 7231 ceiling must not invert
    /// content negotiation. A bare `q=5` should be clamped to 1.0 and so
    /// tie (not outrank) a legitimate `q=1.0` type, with source order
    /// breaking the tie — the over-limit weight gains no priority.
    #[test]
    fn parse_accept_clamps_q_above_one() {
        let parsed = parse_accept("application/json;q=1.0, text/html;q=5");
        assert_eq!(
            parsed,
            vec![
                "application/json;q=1.0".to_string(),
                "text/html;q=5".to_string(),
            ],
            "q=5 must clamp to 1.0 and not outrank an explicit q=1.0 type"
        );
    }

    /// A negative `q` is clamped up to 0.0 (the RFC floor), so it still
    /// sorts last rather than wrapping into an unexpected ordering.
    #[test]
    fn parse_accept_clamps_negative_q_to_floor() {
        let parsed = parse_accept("text/html;q=-3, application/json;q=0.5");
        assert_eq!(
            parsed,
            vec![
                "application/json;q=0.5".to_string(),
                "text/html;q=-3".to_string(),
            ],
            "q=-3 must clamp to 0.0 and sort below a positive-weight type"
        );
    }

    /// A pre-buffered body (typical of CSRF-buffered form requests) must
    /// still honor a tighter per-request cap. A FormRequest overriding
    /// `max_body_bytes` below the buffering middleware's limit must see
    /// the over-limit 413, identical to the streaming arm — the cap is
    /// not silently lost just because the bytes are already in memory.
    #[tokio::test]
    async fn buffered_body_over_cap_is_rejected() {
        let parts = hyper::Request::builder()
            .method("POST")
            .uri("/")
            .body(())
            .expect("build request parts")
            .into_parts()
            .0;
        let req = Request {
            parts,
            body: BodyState::Buffered(Bytes::from_static(&[0u8; 128])),
            params: HashMap::new(),
            route_pattern: None,
            peer_addr: None,
            trusted_proxies: TrustedProxiesConfig::empty(),
        };

        // Use `.err()` rather than `expect_err` so the test doesn't require
        // the Ok variant `(RequestParts, Bytes)` to be `Debug`.
        let err = req
            .body_bytes_with_cap(64)
            .await
            .err()
            .expect("128-byte buffered body must exceed the 64-byte cap");
        assert_eq!(err.status_code(), 413, "over-cap buffered body is 413");
    }

    /// A pre-buffered body within the cap returns its cached bytes
    /// unchanged — the cap check only rejects, it never truncates.
    #[tokio::test]
    async fn buffered_body_within_cap_passes_through() {
        let parts = hyper::Request::builder()
            .method("POST")
            .uri("/")
            .body(())
            .expect("build request parts")
            .into_parts()
            .0;
        let req = Request {
            parts,
            body: BodyState::Buffered(Bytes::from_static(b"hello")),
            params: HashMap::new(),
            route_pattern: None,
            peer_addr: None,
            trusted_proxies: TrustedProxiesConfig::empty(),
        };

        let (_, bytes) = req
            .body_bytes_with_cap(64)
            .await
            .expect("5-byte body within 64-byte cap");
        assert_eq!(bytes.as_ref(), b"hello");
    }

    #[test]
    fn glob_match_literals_and_wildcards() {
        assert!(glob_match("users.*", "users.show"));
        assert!(glob_match("users.*", "users.index"));
        assert!(!glob_match("users.*", "posts.show"));
        assert!(glob_match("admin/*", "admin/users"));
        assert!(glob_match("api/*/users", "api/v1/users"));
        assert!(!glob_match("api/*/users", "api/v1/posts"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "Exact"));
    }

    #[test]
    fn forwarded_for_drops_unparseable_hops() {
        use std::net::{IpAddr, Ipv4Addr};

        let peer = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let parts = hyper::Request::builder()
            .method("GET")
            .uri("/")
            .header(
                "X-Forwarded-For",
                "1.2.3.4, <script>alert(1)</script>, 5.6.7.8",
            )
            .body(())
            .expect("build request parts")
            .into_parts()
            .0;
        let req = Request {
            parts,
            body: BodyState::Buffered(Bytes::new()),
            params: HashMap::new(),
            route_pattern: None,
            peer_addr: Some(peer),
            trusted_proxies: TrustedProxiesConfig::with_ips([peer]),
        };

        // The bogus middle hop is dropped — only parseable IPs (plus the
        // authoritative peer) survive. A spoofed token can't inject an
        // arbitrary string into the chain a consumer might render or log.
        assert_eq!(
            req.ips(),
            vec![
                "1.2.3.4".to_string(),
                "5.6.7.8".to_string(),
                "127.0.0.1".to_string(),
            ],
        );
        // `ip()` returns the first parseable forwarded hop.
        assert_eq!(req.ip().as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn forwarded_for_junk_falls_through_to_peer() {
        use std::net::{IpAddr, Ipv4Addr};

        let peer = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9));
        let parts = hyper::Request::builder()
            .method("GET")
            .uri("/")
            .header("X-Forwarded-For", "not-an-ip, also-garbage")
            .body(())
            .expect("build request parts")
            .into_parts()
            .0;
        let req = Request {
            parts,
            body: BodyState::Buffered(Bytes::new()),
            params: HashMap::new(),
            route_pattern: None,
            peer_addr: Some(peer),
            trusted_proxies: TrustedProxiesConfig::with_ips([peer]),
        };

        // A junk-only forwarded chain can't rotate rate-limit buckets — `ip()`
        // falls through to the authoritative TCP peer instead of echoing junk.
        assert_eq!(req.ip().as_deref(), Some("10.0.0.9"));
        assert_eq!(req.ips(), vec!["10.0.0.9".to_string()]);
    }
}
