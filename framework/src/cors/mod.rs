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

use std::time::Duration;

use async_trait::async_trait;

use crate::Request;
use crate::http::{HttpResponse, Response};
use crate::middleware::{Middleware, Next};

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
#[derive(Debug, Clone)]
pub struct CorsConfig {
    origins: AllowedOrigins,
    methods: Vec<String>,
    headers: AllowedHeaders,
    exposed_headers: Vec<String>,
    allow_credentials: bool,
    max_age: Option<Duration>,
}

fn default_methods() -> Vec<String> {
    ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"]
        .iter()
        .map(|s| s.to_string())
        .collect()
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
            methods: default_methods(),
            headers: AllowedHeaders::Any,
            exposed_headers: Vec::new(),
            allow_credentials: false,
            max_age: None,
        }
    }

    /// Allow ANY origin (`Access-Control-Allow-Origin: *`). Explicit opt-in;
    /// there is no permissive `Default`. Incompatible with credentials per
    /// the Fetch spec — when credentials are enabled the middleware echoes
    /// the specific request origin rather than `*`.
    pub fn any_origin() -> Self {
        Self {
            origins: AllowedOrigins::Any,
            methods: default_methods(),
            headers: AllowedHeaders::Any,
            exposed_headers: Vec::new(),
            allow_credentials: false,
            max_age: None,
        }
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

    /// How long (`Access-Control-Max-Age`) the browser may cache the
    /// preflight result.
    pub fn max_age(mut self, age: Duration) -> Self {
        self.max_age = Some(age);
        self
    }

    /// Whether `origin` is permitted by this policy.
    fn is_origin_allowed(&self, origin: &str) -> bool {
        match &self.origins {
            AllowedOrigins::Any => true,
            AllowedOrigins::List(list) => list.iter().any(|o| o == origin),
        }
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
}
