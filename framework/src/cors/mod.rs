//! Cross-Origin Resource Sharing (CORS) middleware.
//!
//! Browsers enforce the same-origin policy: a page served from
//! `https://app.example` may not read a response from `https://api.example`
//! unless that response carries the right `Access-Control-Allow-*` headers.
//! [`CorsMiddleware`] adds those headers and answers the preflight `OPTIONS`
//! request the browser sends before a non-simple cross-origin call.
//!
//! Same-origin apps (the default Inertia setup) don't need CORS at all — it
//! only matters once a browser on a *different* origin calls your API
//! (public API, separate SPA host, mobile webview, etc.).
//!
//! # Install it globally
//!
//! ```rust,ignore
//! use suprnova::{global_middleware, CorsConfig, CorsMiddleware};
//!
//! global_middleware!(CorsMiddleware::new(
//!     CorsConfig::allow_origins(["https://app.example"])
//!         .allow_credentials(true)
//!         .max_age(std::time::Duration::from_secs(600)),
//! ));
//! ```
//!
//! # Preflight reaches the middleware even on unrouted paths
//!
//! A preflight is `OPTIONS` + an `Access-Control-Request-Method` header, and
//! the router has no `OPTIONS` routes — so a preflight never *matches* a
//! route. The server still runs the global middleware chain for unmatched
//! requests (terminating in a 404), so a globally-installed `CorsMiddleware`
//! sees the preflight and short-circuits it with `204` before the 404 is
//! ever produced. This is why CORS must be installed **globally**, not
//! per-route.
//!
//! # Scope which paths get CORS
//!
//! Laravel's `cors.php` config has a `paths` array (`['api/*', ...]`) that
//! limits CORS application to specific URL patterns. Suprnova mirrors this
//! with [`CorsConfig::paths`]: when set, only matching requests get CORS
//! treatment; everything else passes through untouched. Patterns support
//! `*` as a multi-segment wildcard (Laravel's `Str::is` semantics).
//!
//! ```rust,ignore
//! CorsConfig::allow_origins(["https://app.example"])
//!     .paths(["api/*", "sanctum/csrf-cookie"])
//! ```
//!
//! With no `paths`, CORS runs on every request (the Suprnova default — the
//! cleaner choice when CORS is the only thing this middleware does).
//!
//! # Skip via predicate
//!
//! For request-shape predicates that don't fit a path pattern (e.g. skip
//! based on a header, or only run CORS in production), use
//! [`CorsConfig::skip_when`]:
//!
//! ```rust,ignore
//! CorsConfig::any_origin()
//!     .skip_when(|req| req.header("X-Internal").is_some())
//! ```
//!
//! Mirrors Laravel's `HandleCors::skipWhen(Closure)` but on the policy
//! rather than as global mutable state. Multiple `skip_when` callbacks are
//! ANDed-as-OR: any one returning `true` skips CORS.
//!
//! # Regex origin patterns
//!
//! Laravel's `cors.php` has `allowed_origins_patterns` for regex matching
//! ("allow any `*.example.com` subdomain"). Suprnova surfaces this as
//! [`CorsConfig::allow_origin_patterns`]: anchored regexes that, in addition
//! to the literal [`CorsConfig::allow_origins`] list, count as allowed.
//!
//! ```rust,ignore
//! CorsConfig::allow_origins(["https://app.example"])
//!     .allow_origin_patterns([r"^https://[a-z0-9-]+\.staging\.example$"])
//! ```
//!
//! # No permissive default — pick an origin policy explicitly
//!
//! There is intentionally no `Default` for [`CorsConfig`]. A reflexively
//! permissive CORS policy is a security footgun, so you must choose either a
//! fixed allowlist ([`CorsConfig::allow_origins`]) or, explicitly, any origin
//! ([`CorsConfig::any_origin`]).
//!
//! # Credentials and `*`
//!
//! Per the Fetch spec, `Access-Control-Allow-Origin: *` is invalid together
//! with credentials — the browser rejects it. When
//! [`allow_credentials(true)`](CorsConfig::allow_credentials) is set, the
//! middleware always echoes the specific request `Origin` instead of `*`
//! (and likewise reflects requested headers instead of `*`), so the
//! invalid combination can never be emitted.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;

use crate::Request;
use crate::http::{HttpResponse, Response};
use crate::middleware::{Middleware, Next};

/// Predicate invoked per request to decide if CORS should be skipped.
///
/// Returning `true` short-circuits the middleware and forwards the request
/// directly to the next layer with no CORS handling (no preflight answer,
/// no header decoration). Mirrors Laravel's
/// `HandleCors::skipWhen(Closure)`.
pub type SkipPredicate = Arc<dyn Fn(&Request) -> bool + Send + Sync>;

/// Which origins a [`CorsConfig`] permits.
#[derive(Debug, Clone)]
pub enum AllowedOrigins {
    /// Any origin. Emits `Access-Control-Allow-Origin: *` (or, when
    /// credentials are enabled, echoes the specific request origin).
    Any,
    /// A fixed allowlist. The request `Origin` is echoed back only when it
    /// matches one of these entries exactly (scheme + host + port).
    List(Vec<String>),
}

/// Which request headers a preflight permits in
/// `Access-Control-Allow-Headers`.
#[derive(Debug, Clone)]
pub enum AllowedHeaders {
    /// Reflect whatever the preflight asked for in
    /// `Access-Control-Request-Headers` (or `*` when there is no such
    /// header and credentials are off).
    Any,
    /// A fixed allowlist, echoed verbatim.
    List(Vec<String>),
}

/// CORS policy consumed by [`CorsMiddleware`].
///
/// Build one with [`CorsConfig::allow_origins`] (a fixed allowlist) or
/// [`CorsConfig::any_origin`] (explicit `*`), then refine with the builder
/// methods. See the [module docs](self) for the design rationale.
#[derive(Clone)]
pub struct CorsConfig {
    origins: AllowedOrigins,
    origin_patterns: Vec<Regex>,
    paths: Vec<String>,
    methods: Vec<String>,
    headers: AllowedHeaders,
    exposed_headers: Vec<String>,
    allow_credentials: bool,
    max_age: Option<Duration>,
    skip_callbacks: Vec<SkipPredicate>,
}

impl std::fmt::Debug for CorsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CorsConfig")
            .field("origins", &self.origins)
            .field(
                "origin_patterns",
                &self
                    .origin_patterns
                    .iter()
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("paths", &self.paths)
            .field("methods", &self.methods)
            .field("headers", &self.headers)
            .field("exposed_headers", &self.exposed_headers)
            .field("allow_credentials", &self.allow_credentials)
            .field("max_age", &self.max_age)
            .field("skip_callbacks", &self.skip_callbacks.len())
            .finish()
    }
}

fn default_methods() -> Vec<String> {
    ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Anchor a user-supplied regex with `^` / `$` if missing. Half-anchored
/// patterns produce surprising matches (`https://evil.com/?u=app.example`
/// would match `https://.*\.example`), so we always anchor to whole-string
/// match.
fn anchor_regex(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    if !raw.starts_with('^') {
        out.push('^');
    }
    out.push_str(raw);
    if !raw.ends_with('$') {
        out.push('$');
    }
    out
}

/// Match a Laravel-style URL path pattern against a request path. `*` in
/// the pattern matches any run of characters (including `/`), mirroring
/// Laravel's `Str::is`. A leading `/` on either side is normalized so
/// `"api/*"` and `"/api/*"` both match `"/api/posts"`.
fn path_pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches('/');
    let path = path.trim_start_matches('/');

    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == path;
    }

    // Translate `*` to `.*`, escape every other regex metacharacter.
    let mut re = String::with_capacity(pattern.len() + 4);
    re.push('^');
    for ch in pattern.chars() {
        if ch == '*' {
            re.push_str(".*");
        } else if ch.is_ascii_alphanumeric() || ch == '/' || ch == '-' || ch == '_' {
            re.push(ch);
        } else {
            // Escape regex metacharacters in literal segments.
            re.push('\\');
            re.push(ch);
        }
    }
    re.push('$');
    Regex::new(&re).map(|r| r.is_match(path)).unwrap_or(false)
}

impl CorsConfig {
    /// Allow a fixed list of origins. The request `Origin` is echoed back
    /// only when it exactly matches one of `origins`.
    ///
    /// Methods default to the common set (GET/POST/PUT/PATCH/DELETE/OPTIONS/
    /// HEAD), headers to [`AllowedHeaders::Any`], credentials off, no
    /// `Access-Control-Max-Age`.
    pub fn allow_origins<I, S>(origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            origins: AllowedOrigins::List(origins.into_iter().map(Into::into).collect()),
            origin_patterns: Vec::new(),
            paths: Vec::new(),
            methods: default_methods(),
            headers: AllowedHeaders::Any,
            exposed_headers: Vec::new(),
            allow_credentials: false,
            max_age: None,
            skip_callbacks: Vec::new(),
        }
    }

    /// Allow ANY origin (`Access-Control-Allow-Origin: *`). Explicit opt-in;
    /// there is no permissive `Default`. Incompatible with credentials per
    /// the Fetch spec — when credentials are enabled the middleware echoes
    /// the specific request origin rather than `*`.
    pub fn any_origin() -> Self {
        Self {
            origins: AllowedOrigins::Any,
            origin_patterns: Vec::new(),
            paths: Vec::new(),
            methods: default_methods(),
            headers: AllowedHeaders::Any,
            exposed_headers: Vec::new(),
            allow_credentials: false,
            max_age: None,
            skip_callbacks: Vec::new(),
        }
    }

    /// Restrict CORS to a fixed list of URL path patterns. The Laravel
    /// `cors.php` `paths` config maps directly to this builder. Patterns
    /// support `*` as a multi-segment wildcard ([Laravel's `Str::is`]
    /// semantics — `*` is greedy across `/`).
    ///
    /// With no `paths` set (the default), CORS runs on every request. With
    /// at least one pattern set, only matching requests get CORS treatment
    /// (preflights AND actual-response decoration); everything else flows
    /// through untouched.
    ///
    /// A leading `/` is normalized away so `"api/*"` and `"/api/*"` are
    /// equivalent — matches Laravel's behavior, where path patterns are
    /// host-relative.
    ///
    /// [Laravel's `Str::is`]: https://laravel.com/docs/13.x/strings#method-str-is
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// CorsConfig::allow_origins(["https://app.example"])
    ///     .paths(["api/*", "sanctum/csrf-cookie"])
    /// ```
    pub fn paths<I, S>(mut self, patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.paths = patterns.into_iter().map(Into::into).collect();
        self
    }

    /// Allow origins matching any of the given regex patterns, in addition
    /// to the literal entries in [`CorsConfig::allow_origins`]. Mirrors
    /// Laravel's `allowed_origins_patterns` config knob — useful for
    /// dynamic subdomains (`https://*.example.com`), preview environments,
    /// or per-tenant origins.
    ///
    /// Patterns are anchored automatically: `^` and `$` are prepended /
    /// appended if missing, so `r"https://.*\.example\.com"` and
    /// `r"^https://.*\.example\.com$"` are equivalent.
    ///
    /// # Panics
    ///
    /// Panics if any pattern fails to compile. CORS policy compiles at
    /// startup, so an invalid pattern is a config bug to surface
    /// immediately rather than fail-open at request time.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// CorsConfig::allow_origins(["https://app.example"])
    ///     .allow_origin_patterns([r"^https://[a-z0-9-]+\.staging\.example$"])
    /// ```
    pub fn allow_origin_patterns<I, S>(mut self, patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.origin_patterns = patterns
            .into_iter()
            .map(|p| {
                let raw = p.as_ref();
                let anchored = anchor_regex(raw);
                Regex::new(&anchored)
                    .unwrap_or_else(|e| panic!("invalid CORS origin pattern {raw:?}: {e}"))
            })
            .collect();
        self
    }

    /// Register a predicate that skips CORS for matching requests. Mirrors
    /// Laravel's `HandleCors::skipWhen(Closure)`, but as part of the
    /// policy rather than global mutable state.
    ///
    /// The predicate runs first thing in [`CorsMiddleware::handle`]; on
    /// `true` the request is forwarded directly to the next layer with no
    /// CORS handling. Multiple `skip_when` callbacks may be registered;
    /// any one returning `true` skips CORS.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// CorsConfig::any_origin()
    ///     .skip_when(|req| req.header("X-Internal-Call").is_some())
    /// ```
    pub fn skip_when<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&Request) -> bool + Send + Sync + 'static,
    {
        self.skip_callbacks.push(Arc::new(predicate));
        self
    }

    /// Override the methods advertised in `Access-Control-Allow-Methods`.
    pub fn methods<I, S>(mut self, methods: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.methods = methods.into_iter().map(Into::into).collect();
        self
    }

    /// Restrict `Access-Control-Allow-Headers` to a fixed allowlist instead
    /// of reflecting the preflight's request.
    pub fn allow_headers<I, S>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.headers = AllowedHeaders::List(headers.into_iter().map(Into::into).collect());
        self
    }

    /// Reflect whatever headers the preflight asks for (the default).
    pub fn allow_any_headers(mut self) -> Self {
        self.headers = AllowedHeaders::Any;
        self
    }

    /// Headers to expose to the browser via `Access-Control-Expose-Headers`
    /// (which response headers JS may read on a cross-origin response).
    pub fn expose_headers<I, S>(mut self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.exposed_headers = headers.into_iter().map(Into::into).collect();
        self
    }

    /// Allow credentials (cookies, `Authorization`) on cross-origin
    /// requests. Forces origin/header echoing instead of `*`.
    pub fn allow_credentials(mut self, allow: bool) -> Self {
        self.allow_credentials = allow;
        self
    }

    /// Laravel-named alias for [`Self::allow_credentials`]; matches the
    /// `supports_credentials` key in Laravel's `cors.php` config.
    pub fn supports_credentials(self, allow: bool) -> Self {
        self.allow_credentials(allow)
    }

    /// Laravel-named alias for [`Self::methods`]; matches `allowed_methods`
    /// in Laravel's `cors.php` config.
    pub fn allowed_methods<I, S>(self, methods: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.methods(methods)
    }

    /// Laravel-named alias for [`Self::allow_headers`]; matches
    /// `allowed_headers` in Laravel's `cors.php` config.
    pub fn allowed_headers<I, S>(self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow_headers(headers)
    }

    /// Laravel-named alias for [`Self::expose_headers`]; matches
    /// `exposed_headers` in Laravel's `cors.php` config.
    pub fn exposed_headers<I, S>(self, headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.expose_headers(headers)
    }

    /// Laravel-named alias for [`Self::allow_origin_patterns`]; matches
    /// `allowed_origins_patterns` in Laravel's `cors.php` config.
    pub fn allowed_origins_patterns<I, S>(self, patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.allow_origin_patterns(patterns)
    }

    /// How long (`Access-Control-Max-Age`) the browser may cache the
    /// preflight result.
    pub fn max_age(mut self, age: Duration) -> Self {
        self.max_age = Some(age);
        self
    }

    /// Laravel-style integer-seconds alias for [`Self::max_age`]. The
    /// `cors.php` `max_age` value is in seconds; this overload lets users
    /// pass the same `u64` they would have set in Laravel.
    pub fn max_age_secs(self, seconds: u64) -> Self {
        self.max_age(Duration::from_secs(seconds))
    }

    /// Whether `origin` is permitted by this policy. Consults the literal
    /// allow-list AND any compiled origin patterns; either matching is
    /// enough.
    fn is_origin_allowed(&self, origin: &str) -> bool {
        match &self.origins {
            AllowedOrigins::Any => return true,
            AllowedOrigins::List(list) => {
                if list.iter().any(|o| o == origin) {
                    return true;
                }
            }
        }
        self.origin_patterns.iter().any(|re| re.is_match(origin))
    }

    /// Whether `path` matches any of the configured `paths` patterns. With
    /// no patterns configured, every path matches — Suprnova's default,
    /// since the middleware is opt-in by registration.
    fn path_matches(&self, path: &str) -> bool {
        if self.paths.is_empty() {
            return true;
        }
        self.paths.iter().any(|p| path_pattern_matches(p, path))
    }

    /// Whether any registered `skip_when` predicate matches `request`.
    fn should_skip(&self, request: &Request) -> bool {
        self.skip_callbacks.iter().any(|cb| cb(request))
    }

    /// The `Access-Control-Allow-Origin` value to emit for `origin`, or
    /// `None` when the origin is not allowed. With credentials enabled the
    /// specific origin is always echoed (never `*`).
    fn resolve_acao(&self, origin: &str) -> Option<String> {
        if !self.is_origin_allowed(origin) {
            return None;
        }
        if self.allow_credentials {
            return Some(origin.to_string());
        }
        match &self.origins {
            AllowedOrigins::Any => Some("*".to_string()),
            AllowedOrigins::List(_) => Some(origin.to_string()),
        }
    }

    /// The `Access-Control-Allow-Headers` value for a preflight, given the
    /// browser's `Access-Control-Request-Headers` (`acrh`). `None` means
    /// emit no such header.
    fn resolve_allow_headers(&self, acrh: Option<&str>) -> Option<String> {
        match &self.headers {
            AllowedHeaders::List(list) => Some(list.join(", ")),
            AllowedHeaders::Any => {
                if self.allow_credentials {
                    // `*` is taken literally when credentials are on, so
                    // reflect exactly what was asked for (or nothing).
                    acrh.map(|s| s.to_string())
                } else {
                    acrh.map(|s| s.to_string())
                        .or_else(|| Some("*".to_string()))
                }
            }
        }
    }
}

/// Middleware that applies a [`CorsConfig`]: it answers preflight `OPTIONS`
/// requests with `204` + the negotiated `Access-Control-*` headers, and
/// decorates ordinary cross-origin responses with `Access-Control-Allow-
/// Origin` (plus credentials / exposed-headers / `Vary`).
///
/// Install it **globally** so preflights reach it — see the [module
/// docs](self).
pub struct CorsMiddleware {
    config: CorsConfig,
}

impl CorsMiddleware {
    /// Build the middleware from a policy.
    pub fn new(config: CorsConfig) -> Self {
        Self { config }
    }

    /// Build the `204` preflight response for the given request `origin` and
    /// `Access-Control-Request-Headers` (`acrh`). `Access-Control-*` headers
    /// are emitted only when the origin is allowed; a disallowed origin gets
    /// a bare `204` (and the browser's missing-header check produces the
    /// CORS error, matching the `tower-http` convention). `Vary` is always
    /// set so shared caches key on the request characteristics.
    fn build_preflight(&self, origin: Option<&str>, acrh: Option<&str>) -> HttpResponse {
        let mut resp = HttpResponse::new().status(204);

        if let Some(acao) = origin.and_then(|o| self.config.resolve_acao(o)) {
            resp = resp.header("Access-Control-Allow-Origin", acao);
            if self.config.allow_credentials {
                resp = resp.header("Access-Control-Allow-Credentials", "true");
            }
            resp = resp.header(
                "Access-Control-Allow-Methods",
                self.config.methods.join(", "),
            );
            if let Some(allow_headers) = self.config.resolve_allow_headers(acrh) {
                resp = resp.header("Access-Control-Allow-Headers", allow_headers);
            }
            if let Some(age) = self.config.max_age {
                resp = resp.header("Access-Control-Max-Age", age.as_secs().to_string());
            }
        }

        resp.header(
            "Vary",
            "Origin, Access-Control-Request-Method, Access-Control-Request-Headers",
        )
    }

    /// Decorate an actual (non-preflight) response with CORS headers when
    /// `origin` is allowed; otherwise return it untouched.
    fn decorate_actual(&self, mut resp: HttpResponse, origin: &str) -> HttpResponse {
        let Some(acao) = self.config.resolve_acao(origin) else {
            return resp;
        };
        let is_wildcard = acao == "*";
        resp = resp.header("Access-Control-Allow-Origin", acao);
        if self.config.allow_credentials {
            resp = resp.header("Access-Control-Allow-Credentials", "true");
        }
        if !self.config.exposed_headers.is_empty() {
            resp = resp.header(
                "Access-Control-Expose-Headers",
                self.config.exposed_headers.join(", "),
            );
        }
        // A non-wildcard ACAO varies by Origin, so shared caches must key on
        // it. `*` is identical for every origin, so no `Vary` is needed.
        if !is_wildcard {
            resp = resp.header("Vary", "Origin");
        }
        resp
    }
}

#[async_trait]
impl Middleware for CorsMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Skip-when predicates and `paths` scoping short-circuit the
        // middleware entirely — the request just flows through to the
        // next layer with no CORS treatment. Matches Laravel's
        // `HandleCors`, which returns `$next($request)` early when
        // `hasMatchingPath` is false or a `skipWhen` callback fires.
        if self.config.should_skip(&request) || !self.config.path_matches(request.path()) {
            return next(request).await;
        }

        let origin = request.header("Origin").map(|s| s.to_string());

        // Preflight = OPTIONS carrying Access-Control-Request-Method. A bare
        // OPTIONS is NOT a preflight and passes through to the handler.
        let is_preflight = *request.method() == hyper::Method::OPTIONS
            && request.header("Access-Control-Request-Method").is_some();

        if is_preflight {
            let acrh = request.header("Access-Control-Request-Headers");
            return Ok(self.build_preflight(origin.as_deref(), acrh));
        }

        let response = next(request).await;

        // No Origin header → not a cross-origin request; leave it alone.
        let Some(origin) = origin else {
            return response;
        };
        match response {
            Ok(resp) => Ok(self.decorate_actual(resp, &origin)),
            Err(resp) => Err(self.decorate_actual(resp, &origin)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(resp: &hyper::Response<impl Sized>, name: &str) -> Option<String> {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    }

    #[test]
    fn list_echoes_an_allowed_origin_and_rejects_others() {
        let cfg = CorsConfig::allow_origins(["https://app.example"]);
        assert_eq!(
            cfg.resolve_acao("https://app.example"),
            Some("https://app.example".to_string())
        );
        assert_eq!(cfg.resolve_acao("https://evil.example"), None);
    }

    #[test]
    fn any_origin_emits_wildcard_without_credentials() {
        let cfg = CorsConfig::any_origin();
        assert_eq!(cfg.resolve_acao("https://anything"), Some("*".to_string()));
    }

    #[test]
    fn credentials_force_specific_origin_never_wildcard() {
        // The `*` + credentials combination is invalid per the Fetch spec.
        let cfg = CorsConfig::any_origin().allow_credentials(true);
        assert_eq!(
            cfg.resolve_acao("https://app.example"),
            Some("https://app.example".to_string()),
            "credentials must echo the specific origin, never `*`"
        );
    }

    #[test]
    fn preflight_for_allowed_origin_carries_negotiated_headers() {
        let cfg = CorsConfig::allow_origins(["https://app.example"])
            .allow_credentials(true)
            .max_age(Duration::from_secs(600));
        let mw = CorsMiddleware::new(cfg);

        let resp = mw
            .build_preflight(
                Some("https://app.example"),
                Some("content-type, authorization"),
            )
            .into_hyper();

        assert_eq!(resp.status(), 204);
        assert_eq!(
            header(&resp, "access-control-allow-origin").as_deref(),
            Some("https://app.example")
        );
        assert_eq!(
            header(&resp, "access-control-allow-credentials").as_deref(),
            Some("true")
        );
        assert_eq!(
            header(&resp, "access-control-allow-headers").as_deref(),
            Some("content-type, authorization"),
            "Any-headers + credentials must reflect the requested headers, not `*`"
        );
        assert_eq!(
            header(&resp, "access-control-max-age").as_deref(),
            Some("600")
        );
        assert!(
            header(&resp, "vary")
                .map(|v| v.contains("Origin"))
                .unwrap_or(false)
        );
    }

    #[test]
    fn preflight_for_disallowed_origin_omits_cors_headers() {
        let cfg = CorsConfig::allow_origins(["https://app.example"]);
        let mw = CorsMiddleware::new(cfg);

        let resp = mw
            .build_preflight(Some("https://evil.example"), Some("content-type"))
            .into_hyper();

        assert_eq!(resp.status(), 204);
        assert_eq!(
            header(&resp, "access-control-allow-origin"),
            None,
            "a disallowed origin must NOT receive an Access-Control-Allow-Origin"
        );
    }

    #[test]
    fn actual_response_is_decorated_with_vary_when_not_wildcard() {
        let cfg = CorsConfig::allow_origins(["https://app.example"]).expose_headers(["X-Total"]);
        let mw = CorsMiddleware::new(cfg);

        let resp = mw
            .decorate_actual(
                HttpResponse::json(serde_json::json!({"ok": true})),
                "https://app.example",
            )
            .into_hyper();

        assert_eq!(
            header(&resp, "access-control-allow-origin").as_deref(),
            Some("https://app.example")
        );
        assert_eq!(
            header(&resp, "access-control-expose-headers").as_deref(),
            Some("X-Total")
        );
        assert_eq!(header(&resp, "vary").as_deref(), Some("Origin"));
    }

    #[test]
    fn wildcard_actual_response_skips_vary() {
        let cfg = CorsConfig::any_origin();
        let mw = CorsMiddleware::new(cfg);

        let resp = mw
            .decorate_actual(HttpResponse::text("hi"), "https://anything")
            .into_hyper();

        assert_eq!(
            header(&resp, "access-control-allow-origin").as_deref(),
            Some("*")
        );
        assert_eq!(
            header(&resp, "vary"),
            None,
            "a `*` ACAO is identical for every origin, so no Vary is needed"
        );
    }

    // -- regex origin patterns --------------------------------------------

    #[test]
    fn origin_pattern_matches_wildcard_subdomain() {
        let cfg = CorsConfig::allow_origins(["https://app.example"])
            .allow_origin_patterns([r"https://[a-z0-9-]+\.staging\.example"]);
        assert_eq!(
            cfg.resolve_acao("https://app.example").as_deref(),
            Some("https://app.example"),
            "literal entry still matches"
        );
        assert_eq!(
            cfg.resolve_acao("https://feature-1.staging.example")
                .as_deref(),
            Some("https://feature-1.staging.example"),
            "pattern entry matches"
        );
        assert_eq!(
            cfg.resolve_acao("https://evil.example"),
            None,
            "neither literal nor pattern matches"
        );
    }

    #[test]
    fn origin_patterns_auto_anchor_to_whole_string() {
        // Without anchoring, "https://.*\.example" would naively match
        // "https://evil.com/redirect?u=https://app.example".
        let cfg = CorsConfig::allow_origins(Vec::<String>::new())
            .allow_origin_patterns([r"https://[a-z]+\.example"]);
        assert_eq!(
            cfg.resolve_acao("https://app.example").as_deref(),
            Some("https://app.example")
        );
        assert_eq!(
            cfg.resolve_acao("https://evil.example/?u=https://app.example"),
            None,
            "auto-anchor must reject half-matches"
        );
    }

    #[test]
    fn allowed_origins_patterns_alias_works() {
        // Laravel-named alias delegates to the same surface.
        let cfg = CorsConfig::allow_origins(Vec::<String>::new())
            .allowed_origins_patterns([r"https://api-v\d+\.example"]);
        assert!(cfg.is_origin_allowed("https://api-v2.example"));
        assert!(!cfg.is_origin_allowed("https://api-vX.example"));
    }

    #[test]
    #[should_panic(expected = "invalid CORS origin pattern")]
    fn invalid_origin_pattern_panics_at_config_time() {
        // Fail loud at boot, not silently at request time.
        let _ = CorsConfig::any_origin().allow_origin_patterns(["[unclosed-class"]);
    }

    // -- paths scoping ----------------------------------------------------

    #[test]
    fn path_pattern_handles_wildcard_suffix() {
        assert!(path_pattern_matches("api/*", "/api/users"));
        assert!(path_pattern_matches("api/*", "/api/users/42"));
        assert!(!path_pattern_matches("api/*", "/web/users"));
    }

    #[test]
    fn path_pattern_handles_wildcard_anywhere() {
        assert!(path_pattern_matches("api/*/posts", "/api/v2/posts"));
        assert!(!path_pattern_matches("api/*/posts", "/api/v2/comments"));
    }

    #[test]
    fn path_pattern_handles_literal_match() {
        assert!(path_pattern_matches(
            "sanctum/csrf-cookie",
            "/sanctum/csrf-cookie"
        ));
        assert!(!path_pattern_matches(
            "sanctum/csrf-cookie",
            "/sanctum/csrf-token"
        ));
    }

    #[test]
    fn path_pattern_normalizes_leading_slash() {
        // Both `"api/*"` and `"/api/*"` work the same way.
        assert!(path_pattern_matches("/api/*", "/api/users"));
        assert!(path_pattern_matches("api/*", "api/users"));
    }

    #[test]
    fn path_pattern_lone_star_matches_anything() {
        assert!(path_pattern_matches("*", "/anything/at/all"));
        assert!(path_pattern_matches("*", "/"));
    }

    #[test]
    fn path_matches_returns_true_when_no_paths_configured() {
        let cfg = CorsConfig::any_origin();
        assert!(cfg.path_matches("/anything"));
    }

    #[test]
    fn path_matches_filters_to_configured_paths() {
        let cfg = CorsConfig::any_origin().paths(["api/*", "sanctum/csrf-cookie"]);
        assert!(cfg.path_matches("/api/users"));
        assert!(cfg.path_matches("/sanctum/csrf-cookie"));
        assert!(!cfg.path_matches("/web/login"));
    }

    // -- helper utilities -------------------------------------------------

    #[test]
    fn anchor_regex_preserves_existing_anchors() {
        assert_eq!(anchor_regex("^https://app$"), "^https://app$");
        assert_eq!(anchor_regex("^https://app"), "^https://app$");
        assert_eq!(anchor_regex("https://app$"), "^https://app$");
        assert_eq!(anchor_regex("https://app"), "^https://app$");
    }
}
