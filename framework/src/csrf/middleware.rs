//! CSRF protection middleware

use crate::Request;
use crate::http::{Cookie, HttpResponse, Response, SameSite};
use crate::middleware::{Middleware, Next};
use crate::session::get_csrf_token;
use async_trait::async_trait;
use std::time::Duration;

/// Maximum bytes we will buffer from a form-urlencoded request body to
/// look for the `_token` field. A `_token` field is a 40-char hex
/// string; the rest of the form might contain reasonably-sized fields
/// (login form, contact form, etc.). 64 KiB is comfortable for those
/// cases and small enough that a malicious large form won't pin the
/// server's memory waiting on CSRF validation.
const CSRF_BODY_BUFFER_CAP: usize = 64 * 1024;

/// How `CsrfMiddleware` treats the browser's `Sec-Fetch-Site` header.
///
/// Mirrors Laravel 13's `PreventRequestForgery::$allowSameSite` /
/// `$originOnly` static knobs, exposed here as a single explicit enum
/// so the policy is a value, not a pair of bools.
///
/// Defaults to [`OriginPolicy::Disabled`] — Sec-Fetch-Site is ignored
/// and only token validation runs. This matches the historical
/// Suprnova behavior; opting in to origin verification is a one-call
/// switch via [`CsrfMiddleware::allow_same_site`] /
/// [`CsrfMiddleware::origin_only`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginPolicy {
    /// Don't read `Sec-Fetch-Site` at all. Token validation is the
    /// only check.
    Disabled,
    /// `same-origin` short-circuits to pass. Anything else falls
    /// through to token validation. Matches Laravel's default when
    /// the middleware is enabled (`allowSameSite=false`,
    /// `originOnly=false`).
    SameOriginOnly,
    /// `same-origin` *and* `same-site` short-circuit to pass.
    /// Useful for subdomain-fanout deploys (e.g. `dashboard.example.com`
    /// accepting requests from `example.com`). Matches Laravel's
    /// `allowSameSite=true`.
    AllowSameSite,
    /// Origin verification is the *only* gate — if `Sec-Fetch-Site`
    /// is missing or doesn't match, reject with **403** (matching
    /// Laravel's `OriginMismatchException`, distinct from the 419
    /// `TokenMismatchException`). Token validation is skipped.
    /// Matches Laravel's `originOnly=true`.
    OriginOnly,
}

impl OriginPolicy {
    /// Returns whether `same-site` is allowed under this policy.
    fn allows_same_site(self) -> bool {
        matches!(self, OriginPolicy::AllowSameSite)
    }
}

/// Outcome of consulting `Sec-Fetch-Site` for the current request.
enum OriginCheck {
    /// Header matched the policy — let the request through without
    /// touching the token.
    Pass,
    /// Header didn't match. In `OriginOnly` mode this is fatal; in
    /// the other modes the request falls through to token validation.
    Fail,
}

/// CSRF protection middleware
///
/// Validates CSRF tokens on state-changing requests (POST, PUT, PATCH, DELETE).
///
/// # Token Sources
///
/// The middleware looks for the CSRF token in the following order:
/// 1. `X-CSRF-TOKEN` header (used by Inertia.js)
/// 2. `X-XSRF-TOKEN` header (Laravel convention; reads the
///    `XSRF-TOKEN` cookie value the framework issued on the response)
/// 3. `_token` form field (traditional forms)
///
/// # Origin verification (Laravel 13's `PreventRequestForgery`)
///
/// In addition to (or instead of) token validation, this middleware
/// can consult the browser's `Sec-Fetch-Site` header — modern
/// browsers set it on every request and a same-origin request can be
/// allowed without any token round-trip. See [`OriginPolicy`] and
/// [`CsrfMiddleware::allow_same_site`] / [`CsrfMiddleware::origin_only`].
///
/// # XSRF-TOKEN cookie
///
/// On every response (read or write), the middleware attaches an
/// `XSRF-TOKEN` cookie containing the current session's CSRF token.
/// This is the Laravel convention: SPA libraries (Axios, Angular)
/// read the cookie via JavaScript and echo the value back in the
/// `X-XSRF-TOKEN` header on subsequent state-changing requests. The
/// cookie is `HttpOnly=false` by design — it has to be readable by
/// the SPA — and is therefore set as plaintext (no `Crypt`
/// round-trip) so the JS-side value matches what the middleware
/// compares server-side.
///
/// # Usage
///
/// ```rust,ignore
/// use suprnova::{global_middleware, CsrfMiddleware};
///
/// global_middleware!(CsrfMiddleware::new());
/// ```
pub struct CsrfMiddleware {
    /// HTTP methods that require CSRF validation
    protected_methods: Vec<&'static str>,
    /// Paths to exclude from CSRF validation (e.g., webhooks)
    except: Vec<String>,
    /// How to treat `Sec-Fetch-Site`. Default: [`OriginPolicy::Disabled`].
    origin_policy: OriginPolicy,
    /// Whether to attach the `XSRF-TOKEN` cookie to outgoing responses.
    add_xsrf_cookie: bool,
    /// Cookie lifetime. Mirrors Laravel's `session.lifetime` window
    /// (`Cookie('XSRF-TOKEN', token, availableAt(60 * lifetime), ...)`).
    /// Defaults to 2 hours, matching the session-config default.
    xsrf_cookie_lifetime: Duration,
    /// `Path` attribute on the XSRF cookie. Defaults to `/`.
    xsrf_cookie_path: String,
    /// `Domain` attribute on the XSRF cookie. Defaults to unset
    /// (browser uses the request host).
    xsrf_cookie_domain: Option<String>,
    /// `Secure` flag on the XSRF cookie. Defaults to `true`.
    xsrf_cookie_secure: bool,
    /// `SameSite` attribute on the XSRF cookie. Defaults to `Lax`.
    xsrf_cookie_same_site: SameSite,
}

impl CsrfMiddleware {
    /// Create a new CSRF middleware with default settings
    ///
    /// Protects: POST, PUT, PATCH, DELETE.
    ///
    /// Origin verification is **off** by default (token validation
    /// only); use [`allow_same_site`](Self::allow_same_site) or
    /// [`origin_only`](Self::origin_only) to enable it.
    pub fn new() -> Self {
        Self {
            protected_methods: vec!["POST", "PUT", "PATCH", "DELETE"],
            except: Vec::new(),
            origin_policy: OriginPolicy::Disabled,
            add_xsrf_cookie: true,
            xsrf_cookie_lifetime: Duration::from_secs(120 * 60),
            xsrf_cookie_path: "/".to_string(),
            xsrf_cookie_domain: None,
            xsrf_cookie_secure: true,
            xsrf_cookie_same_site: SameSite::Lax,
        }
    }

    /// Add paths to exclude from CSRF validation
    ///
    /// Useful for webhooks or API endpoints that use other authentication.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let csrf = CsrfMiddleware::new()
    ///     .except(vec!["/webhooks/*", "/api/external/*"]);
    /// ```
    pub fn except(mut self, paths: Vec<impl Into<String>>) -> Self {
        self.except = paths.into_iter().map(|p| p.into()).collect();
        self
    }

    /// Enable origin verification with `same-site` allowed in addition
    /// to `same-origin`. Useful when an app serves multiple subdomains
    /// of the same registrable domain (e.g. `dashboard.example.com`
    /// accepting requests from `app.example.com`).
    ///
    /// Mirrors Laravel's `PreventRequestForgery::allowSameSite(true)`.
    ///
    /// Same-origin requests pass without token validation; everything
    /// else falls through to the token check.
    pub fn allow_same_site(mut self) -> Self {
        self.origin_policy = OriginPolicy::AllowSameSite;
        self
    }

    /// Switch to origin-only verification — `Sec-Fetch-Site` is the
    /// *only* gate, token validation is skipped, and a missing or
    /// mismatched header rejects with **403**.
    ///
    /// Mirrors Laravel's `PreventRequestForgery::useOriginOnly(true)`.
    /// Use this for purely API-driven apps where token round-tripping
    /// adds nothing over what the browser's same-origin header gives
    /// you for free.
    ///
    /// **Caveat (from the Laravel docs):** `Sec-Fetch-Site` is only
    /// emitted by browsers over HTTPS. If your app runs over plain
    /// HTTP, origin verification is unavailable and every state-changing
    /// request will reject with 403.
    pub fn origin_only(mut self) -> Self {
        self.origin_policy = OriginPolicy::OriginOnly;
        self
    }

    /// Lower-level: set the [`OriginPolicy`] explicitly. Most callers
    /// should use [`allow_same_site`](Self::allow_same_site) /
    /// [`origin_only`](Self::origin_only) instead.
    pub fn with_origin_policy(mut self, policy: OriginPolicy) -> Self {
        self.origin_policy = policy;
        self
    }

    /// Disable the `XSRF-TOKEN` cookie attachment on responses.
    /// Mirrors Laravel's `PreventRequestForgery::$addHttpCookie = false`.
    /// Most apps want this on — Axios and Angular pick the cookie up
    /// automatically — but a pure server-rendered app that issues the
    /// token via `{{ csrf_meta_tag() }}` doesn't need it.
    pub fn without_xsrf_cookie(mut self) -> Self {
        self.add_xsrf_cookie = false;
        self
    }

    /// Set the `Path` attribute on the `XSRF-TOKEN` cookie.
    pub fn xsrf_cookie_path(mut self, path: impl Into<String>) -> Self {
        self.xsrf_cookie_path = path.into();
        self
    }

    /// Set the `Domain` attribute on the `XSRF-TOKEN` cookie.
    pub fn xsrf_cookie_domain(mut self, domain: impl Into<String>) -> Self {
        self.xsrf_cookie_domain = Some(domain.into());
        self
    }

    /// Set the `Secure` flag on the `XSRF-TOKEN` cookie. Defaults to
    /// `true`; you'll want to flip this off for local HTTP-only
    /// development.
    pub fn xsrf_cookie_secure(mut self, secure: bool) -> Self {
        self.xsrf_cookie_secure = secure;
        self
    }

    /// Set the `SameSite` attribute on the `XSRF-TOKEN` cookie.
    pub fn xsrf_cookie_same_site(mut self, value: SameSite) -> Self {
        self.xsrf_cookie_same_site = value;
        self
    }

    /// Set the `XSRF-TOKEN` cookie's lifetime. Defaults to 2 hours
    /// (matching the session-config default — Laravel sets the
    /// cookie's `availableAt(60 * session.lifetime)`).
    pub fn xsrf_cookie_lifetime(mut self, duration: Duration) -> Self {
        self.xsrf_cookie_lifetime = duration;
        self
    }

    /// Sync the `XSRF-TOKEN` cookie's attributes from a
    /// [`crate::session::SessionConfig`]. Mirrors Laravel's
    /// `newCookie()`, which reads `path`/`domain`/`secure`/`same_site`/
    /// `lifetime` from `config('session')`.
    ///
    /// **Why this exists:** the constructor defaults match Suprnova's
    /// `SessionConfig::default()` exactly, but an app that *overrides*
    /// `SESSION_SECURE`/`SESSION_DOMAIN`/`SESSION_SAME_SITE`/
    /// `SESSION_LIFETIME` would otherwise get a session cookie that
    /// respects those overrides and an XSRF cookie that silently
    /// doesn't. Calling this at boot keeps the two in lockstep:
    ///
    /// ```rust,ignore
    /// let session_config = SessionConfig::from_env();
    /// let csrf = CsrfMiddleware::new().with_session_config(&session_config);
    /// global_middleware!(SessionMiddleware::new(session_config));
    /// global_middleware!(csrf);
    /// ```
    ///
    /// The `SessionConfig::cookie_same_site` string is parsed
    /// case-insensitively into [`SameSite`] using the same matrix as
    /// the session middleware itself
    /// (`"strict"` → `Strict`, `"none"` → `None`, anything else →
    /// `Lax`), so an unknown value gets the same fallback in both
    /// places.
    pub fn with_session_config(mut self, config: &crate::session::SessionConfig) -> Self {
        self.xsrf_cookie_path = config.cookie_path.clone();
        self.xsrf_cookie_domain = config.cookie_domain.clone();
        self.xsrf_cookie_secure = config.cookie_secure;
        self.xsrf_cookie_lifetime = config.lifetime;
        self.xsrf_cookie_same_site = match config.cookie_same_site.to_lowercase().as_str() {
            "strict" => SameSite::Strict,
            "none" => SameSite::None,
            _ => SameSite::Lax,
        };
        self
    }

    /// Check if a path should be excluded from CSRF validation
    fn is_excluded(&self, path: &str) -> bool {
        for pattern in &self.except {
            if pattern.ends_with('*') {
                let prefix = &pattern[..pattern.len() - 1];
                if path.starts_with(prefix) {
                    return true;
                }
            } else if pattern == path {
                return true;
            }
        }
        false
    }

    /// Consult `Sec-Fetch-Site` against the configured policy.
    ///
    /// Mirrors Laravel's `PreventRequestForgery::hasValidOrigin`:
    /// `same-origin` always passes; `same-site` passes only when
    /// [`OriginPolicy::AllowSameSite`] is set; everything else fails.
    fn check_origin(&self, request: &Request) -> OriginCheck {
        if matches!(self.origin_policy, OriginPolicy::Disabled) {
            return OriginCheck::Fail;
        }
        match request.header("Sec-Fetch-Site") {
            Some("same-origin") => OriginCheck::Pass,
            Some("same-site") if self.origin_policy.allows_same_site() => OriginCheck::Pass,
            _ => OriginCheck::Fail,
        }
    }

    /// Whether to attach the `XSRF-TOKEN` cookie on the response.
    /// Disabled in `OriginOnly` mode (Laravel's
    /// `shouldAddXsrfTokenCookie` returns `false` when token-based
    /// verification is off).
    fn should_attach_xsrf_cookie(&self) -> bool {
        if matches!(self.origin_policy, OriginPolicy::OriginOnly) {
            return false;
        }
        self.add_xsrf_cookie
    }

    /// Build the `XSRF-TOKEN` cookie for the current session.
    ///
    /// The cookie is **not** `HttpOnly` — JavaScript on the page has
    /// to read it to echo the value back in `X-XSRF-TOKEN`. That's
    /// Laravel's documented behavior too.
    ///
    /// **Documented divergence:** Laravel encrypts the cookie value
    /// (`Illuminate\Cookie\Middleware\EncryptCookies` runs in front
    /// of CSRF in the global stack). Suprnova ships the token as
    /// plaintext so the JS-readable value matches what the middleware
    /// compares server-side. See `docs/parity/csrf.md` (`Diverged`
    /// row for `XSRF-TOKEN` cookie encryption) for the rationale.
    fn build_xsrf_cookie(&self, token: &str) -> Cookie {
        let mut cookie = Cookie::new("XSRF-TOKEN", token)
            .http_only(false)
            .secure(self.xsrf_cookie_secure)
            .same_site(self.xsrf_cookie_same_site.clone())
            .path(self.xsrf_cookie_path.clone())
            .max_age(self.xsrf_cookie_lifetime);
        if let Some(domain) = self.xsrf_cookie_domain.clone() {
            cookie = cookie.domain(domain);
        }
        cookie
    }

    /// Attach the `XSRF-TOKEN` cookie to the response, if policy
    /// allows and a session token exists. Mirrors Laravel's
    /// `addCookieToResponse` running inside the `tap()` after `next`.
    fn maybe_attach_xsrf_cookie(&self, response: Response) -> Response {
        if !self.should_attach_xsrf_cookie() {
            return response;
        }
        let Some(token) = get_csrf_token() else {
            return response;
        };
        let cookie = self.build_xsrf_cookie(&token);
        match response {
            Ok(http) => Ok(http.cookie(cookie)),
            Err(http) => Err(http.cookie(cookie)),
        }
    }
}

impl Default for CsrfMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for CsrfMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let method = request.method().as_str();

        // Reading verbs (GET/HEAD/OPTIONS) are never token-checked.
        // We still run through the bottom of the function so the
        // XSRF-TOKEN cookie gets attached to read responses — that's
        // how SPA clients ever acquire the cookie in the first place.
        let is_reading = !self.protected_methods.contains(&method);

        // Excluded paths bypass both origin and token checks, but
        // still get the XSRF cookie so a webhook handler that later
        // renders an HTML page (uncommon, but possible) still hands
        // out a usable token.
        let is_excluded = self.is_excluded(request.path());

        // Fast path: no validation needed. Run the inner stack, then
        // attach the cookie on the way out.
        if is_reading || is_excluded {
            let response = next(request).await;
            return self.maybe_attach_xsrf_cookie(response);
        }

        // Origin verification (Laravel 13's `PreventRequestForgery`).
        // - `Disabled` short-circuits with `Fail` and we fall straight
        //   to token validation.
        // - `SameOriginOnly` / `AllowSameSite` pass on a matching
        //   header, otherwise fall through to token validation.
        // - `OriginOnly` makes the header the *only* gate: a fail
        //   here is a 403, and token validation is skipped entirely.
        match self.check_origin(&request) {
            OriginCheck::Pass => {
                let response = next(request).await;
                return self.maybe_attach_xsrf_cookie(response);
            }
            OriginCheck::Fail if matches!(self.origin_policy, OriginPolicy::OriginOnly) => {
                return reject_origin_mismatch();
            }
            OriginCheck::Fail => { /* fall through to token validation */ }
        }

        // Get expected token from session. Returning 419 here matches
        // Laravel: `tokensMatch()` returns false (→ 419) when the
        // session token isn't present. A 500 would imply a server
        // misconfiguration where the failure is actually
        // request-shaped (no session cookie, expired session,
        // middleware misordering all surface the same).
        let expected_token = match get_csrf_token() {
            Some(token) => token,
            None => return reject_with_419(),
        };

        // Header tokens (AJAX / Inertia / framework conventions) are
        // always checked first — they don't require body buffering.
        if let Some(token) = request
            .header("X-CSRF-TOKEN")
            .or_else(|| request.header("X-XSRF-TOKEN"))
        {
            if constant_time_compare(token, &expected_token) {
                let response = next(request).await;
                return self.maybe_attach_xsrf_cookie(response);
            }
            // Header was present but wrong — reject without parsing the
            // body. A correct client picks one location for the token;
            // we don't combine header + body to avoid token-splitting
            // surprises.
            return reject_with_419();
        }

        // No header — for `application/x-www-form-urlencoded` bodies
        // we honor the documented `_token` field (the value emitted by
        // `csrf_field()` in HTML forms). We buffer the body so the
        // downstream handler can still read its form data.
        let is_form_body = request
            .content_type()
            .map(|ct| ct.starts_with("application/x-www-form-urlencoded"))
            .unwrap_or(false);

        if !is_form_body {
            return reject_with_419();
        }

        // Buffer the body. CSRF_BODY_BUFFER_CAP caps this at 64 KiB —
        // forms with `_token` are well under that, and a malicious large
        // form won't pin memory on CSRF validation alone.
        let request = match request.buffer_body(CSRF_BODY_BUFFER_CAP).await {
            Ok(r) => r,
            Err(_) => return reject_with_419(),
        };

        let Some(body) = request.cached_body() else {
            return reject_with_419();
        };

        // Parse `_token=...` out of the form bag. `form_urlencoded::parse`
        // URL-decodes values; the token is hex so decoding is a no-op,
        // but using the parser keeps us consistent with how `req.form()`
        // would later see the same body.
        let token_field = url::form_urlencoded::parse(body).find_map(|(k, v)| {
            if k == "_token" {
                Some(v.into_owned())
            } else {
                None
            }
        });

        match token_field {
            Some(token) if constant_time_compare(&token, &expected_token) => {
                let response = next(request).await;
                self.maybe_attach_xsrf_cookie(response)
            }
            _ => reject_with_419(),
        }
    }
}

fn reject_with_419() -> Response {
    Err(HttpResponse::json(serde_json::json!({
        "message": "CSRF token mismatch."
    }))
    .status(419))
}

fn reject_origin_mismatch() -> Response {
    // Mirrors Laravel's `OriginMismatchException`, which renders as
    // 403. Distinct from the 419 token-mismatch path so clients can
    // tell the two failure modes apart.
    Err(HttpResponse::json(serde_json::json!({
        "message": "Origin mismatch."
    }))
    .status(403))
}

/// Constant-time string comparison to prevent timing attacks
///
/// This ensures an attacker can't determine how much of the token is correct
/// based on response time.
fn constant_time_compare(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }

    a.bytes()
        .zip(b.bytes())
        .fold(0, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_time_compare() {
        assert!(constant_time_compare("abc123", "abc123"));
        assert!(!constant_time_compare("abc123", "abc124"));
        assert!(!constant_time_compare("abc123", "abc12"));
        assert!(!constant_time_compare("", "a"));
    }

    #[test]
    fn test_is_excluded() {
        let csrf = CsrfMiddleware::new().except(vec!["/webhooks/*", "/api/public"]);

        assert!(csrf.is_excluded("/webhooks/stripe"));
        assert!(csrf.is_excluded("/webhooks/github/events"));
        assert!(csrf.is_excluded("/api/public"));
        assert!(!csrf.is_excluded("/api/private"));
        assert!(!csrf.is_excluded("/login"));
    }

    #[test]
    fn test_origin_policy_default_is_disabled() {
        // Backwards-compat with prior Suprnova behavior: opting in to
        // origin verification is a deliberate one-call switch, never
        // a silent default that could break apps that already ship
        // tokens but no Sec-Fetch-Site.
        let csrf = CsrfMiddleware::new();
        assert_eq!(csrf.origin_policy, OriginPolicy::Disabled);
    }

    #[test]
    fn test_origin_policy_builders() {
        let csrf = CsrfMiddleware::new().allow_same_site();
        assert_eq!(csrf.origin_policy, OriginPolicy::AllowSameSite);
        let csrf = CsrfMiddleware::new().origin_only();
        assert_eq!(csrf.origin_policy, OriginPolicy::OriginOnly);
        let csrf = CsrfMiddleware::new().with_origin_policy(OriginPolicy::SameOriginOnly);
        assert_eq!(csrf.origin_policy, OriginPolicy::SameOriginOnly);
    }

    #[test]
    fn test_should_attach_xsrf_cookie_off_in_origin_only_mode() {
        // Laravel: `shouldAddXsrfTokenCookie()` returns false when
        // origin-only verification is in effect — there's no token
        // round-trip to feed.
        let csrf = CsrfMiddleware::new().origin_only();
        assert!(!csrf.should_attach_xsrf_cookie());
    }

    #[test]
    fn test_should_attach_xsrf_cookie_respects_explicit_off_switch() {
        let csrf = CsrfMiddleware::new().without_xsrf_cookie();
        assert!(!csrf.should_attach_xsrf_cookie());
    }

    #[test]
    fn test_with_session_config_syncs_xsrf_cookie_attributes() {
        // Apps that override SESSION_* env vars should not get an
        // XSRF cookie that silently diverges from the session cookie.
        // `with_session_config` is the explicit alignment hook.
        let cfg = crate::session::SessionConfig {
            cookie_path: "/app".to_string(),
            cookie_domain: Some(".example.com".to_string()),
            cookie_secure: false,
            cookie_same_site: "Strict".to_string(),
            lifetime: Duration::from_secs(15 * 60),
            ..crate::session::SessionConfig::default()
        };
        let csrf = CsrfMiddleware::new().with_session_config(&cfg);
        let cookie = csrf.build_xsrf_cookie("tok").to_header_value();
        assert!(cookie.contains("Path=/app"), "{cookie}");
        assert!(cookie.contains("Domain=.example.com"), "{cookie}");
        assert!(cookie.contains("SameSite=Strict"), "{cookie}");
        // secure=false → no Secure (unless SameSite=None forces it).
        assert!(!cookie.contains("Secure"), "{cookie}");
        assert!(cookie.contains("Max-Age=900"), "{cookie}");
    }

    #[test]
    fn test_with_session_config_maps_same_site_strings_like_session_middleware() {
        let mk = |s: &str| {
            let mut cfg = crate::session::SessionConfig::default();
            cfg.cookie_same_site = s.to_string();
            CsrfMiddleware::new()
                .with_session_config(&cfg)
                .build_xsrf_cookie("t")
                .to_header_value()
        };
        assert!(mk("Strict").contains("SameSite=Strict"));
        assert!(mk("strict").contains("SameSite=Strict"));
        assert!(mk("None").contains("SameSite=None"));
        assert!(mk("none").contains("SameSite=None"));
        // Unknown / empty / typo all fall back to Lax — matches the
        // session middleware's parser, so a fat-fingered env var
        // produces the same result in both places.
        assert!(mk("Lax").contains("SameSite=Lax"));
        assert!(mk("").contains("SameSite=Lax"));
        assert!(mk("bogus").contains("SameSite=Lax"));
    }

    #[test]
    fn test_build_xsrf_cookie_is_js_readable_plaintext() {
        // The cookie has to be JS-readable for SPAs to echo it in
        // X-XSRF-TOKEN — that's the entire point of the Laravel
        // convention. HttpOnly=false is therefore non-negotiable.
        let csrf = CsrfMiddleware::new();
        let cookie = csrf.build_xsrf_cookie("token-12345");
        let header = cookie.to_header_value();
        assert!(header.starts_with("XSRF-TOKEN=token-12345"));
        assert!(!header.contains("HttpOnly"), "{header}");
        // Defaults: Path=/, Secure, SameSite=Lax, Max-Age=7200.
        assert!(header.contains("Path=/"), "{header}");
        assert!(header.contains("Secure"), "{header}");
        assert!(header.contains("SameSite=Lax"), "{header}");
        assert!(header.contains("Max-Age=7200"), "{header}");
    }

    // ----------------------------------------------------------------
    // Regression: HIGH audit finding `csrf` #335 — documented `_token`
    // form-field validation was not implemented; only headers were read.
    //
    // These tests install a fake session in `SESSION_CONTEXT`, drive a
    // real `Request` through `CsrfMiddleware::handle`, and verify:
    //   (a) a matching `_token` in a form-urlencoded body passes
    //   (b) a wrong `_token` rejects with 419
    //   (c) downstream handler still sees the full form body after the
    //       middleware buffered it (the load-bearing piece — without
    //       this, the fix moved the bug instead of solving it)
    //
    // The same harness now also exercises the Laravel 13 origin-check
    // path (Sec-Fetch-Site) and the XSRF-TOKEN cookie issuance.
    // ----------------------------------------------------------------

    use crate::session::middleware::SESSION_CONTEXT;
    use crate::session::store::SessionData;
    use http_body_util::{BodyExt, Empty, Full};
    use hyper::body::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    /// Outcome from a one-shot driven request: response status, the
    /// downstream handler's view of the form, and the raw response
    /// headers so origin/cookie tests can inspect Set-Cookie etc.
    struct Driven {
        status: u16,
        form_fields: std::collections::HashMap<String, String>,
        headers: hyper::HeaderMap,
    }

    /// Spawn a one-shot hyper server that scopes a session containing
    /// `expected_token`, runs `mw` around a handler that records what
    /// form fields it saw, and returns the response.
    async fn drive_request(
        mw: Arc<CsrfMiddleware>,
        expected_token: &str,
        builder: hyper::http::request::Builder,
        body: Option<String>,
    ) -> Driven {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();

        let captured: Arc<Mutex<std::collections::HashMap<String, String>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let server_captured = captured.clone();

        let expected_token = expected_token.to_string();
        let server_mw = mw.clone();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);
            let server_captured = server_captured.clone();
            let expected_token = expected_token.clone();
            let server_mw = server_mw.clone();
            let service = service_fn(move |hyper_req: hyper::Request<hyper::body::Incoming>| {
                let server_captured = server_captured.clone();
                let expected_token = expected_token.clone();
                let server_mw = server_mw.clone();
                async move {
                    let session = SessionData {
                        csrf_token: expected_token,
                        ..Default::default()
                    };
                    let slot = Arc::new(Mutex::new(Some(session)));

                    let response = SESSION_CONTEXT
                        .scope(slot, async move {
                            let req = Request::new(hyper_req);
                            let next: Next = Arc::new(move |req| {
                                let server_captured = server_captured.clone();
                                Box::pin(async move {
                                    // Reading verbs (GET) have no body
                                    // to parse — only buffer when the
                                    // request actually carries form data.
                                    let is_form = req
                                        .content_type()
                                        .map(|ct| {
                                            ct.starts_with("application/x-www-form-urlencoded")
                                        })
                                        .unwrap_or(false);
                                    if is_form {
                                        let (_, bytes) = req.body_bytes().await?;
                                        let mut map = server_captured.lock().unwrap();
                                        for (k, v) in
                                            url::form_urlencoded::parse(&bytes).into_owned()
                                        {
                                            map.insert(k, v);
                                        }
                                    }
                                    Ok(HttpResponse::text("ok"))
                                })
                            });
                            server_mw.handle(req, next).await
                        })
                        .await;

                    let http = response.unwrap_or_else(|e| e);
                    Ok::<_, Infallible>(http.into_hyper())
                }
            });
            let _ = http1::Builder::new().serve_connection(io, service).await;
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let io = TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let body_bytes = body.unwrap_or_default();
        let mut req_builder = builder;
        if !body_bytes.is_empty() {
            req_builder = req_builder.header("content-length", body_bytes.len().to_string());
        }
        let req = req_builder
            .body(Full::new(Bytes::from(body_bytes)))
            .unwrap();

        let resp = sender.send_request(req).await.unwrap();
        let status = resp.status().as_u16();
        let (parts, body) = resp.into_parts();
        let _ = body.collect().await.unwrap();
        let form_fields = captured.lock().unwrap().clone();
        Driven {
            status,
            form_fields,
            headers: parts.headers,
        }
    }

    async fn drive_form_post(
        expected_token: &str,
        form_body: String,
    ) -> (u16, std::collections::HashMap<String, String>) {
        let mw = Arc::new(CsrfMiddleware::new());
        let builder = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost/login")
            .header("content-type", "application/x-www-form-urlencoded");
        let driven = drive_request(mw, expected_token, builder, Some(form_body)).await;
        (driven.status, driven.form_fields)
    }

    #[tokio::test]
    async fn form_post_with_matching_token_in_body_passes_and_handler_sees_body() {
        // The load-bearing regression test: a real HTTP POST with
        // _token in the body must (a) pass CSRF validation and (b)
        // leave the form body intact for the downstream handler.
        let token = "matching-token-fixture-1234567890";
        let body = format!("_token={token}&username=alice&password=hunter2");

        let (status, fields) = drive_form_post(token, body).await;

        assert_eq!(
            status, 200,
            "form POST with matching _token must pass CSRF (no 419)"
        );
        assert_eq!(
            fields.get("username").map(|s| s.as_str()),
            Some("alice"),
            "downstream handler must still see the form body after CSRF \
             buffered it — without this, we moved the bug instead of fixing it"
        );
        assert_eq!(fields.get("password").map(|s| s.as_str()), Some("hunter2"));
        assert_eq!(
            fields.get("_token").map(|s| s.as_str()),
            Some(token),
            "the _token field stays in the form bag for the handler — \
             CSRF doesn't strip it"
        );
    }

    #[tokio::test]
    async fn form_post_with_wrong_token_in_body_rejects_with_419() {
        let session_token = "real-session-token-xyz";
        let body = "_token=wrong-attacker-token&action=transfer".to_string();

        let (status, _fields) = drive_form_post(session_token, body).await;

        assert_eq!(
            status, 419,
            "form POST with mismatched _token must reject with 419"
        );
    }

    #[tokio::test]
    async fn form_post_with_no_token_at_all_rejects_with_419() {
        let session_token = "real-session-token-xyz";
        let body = "action=transfer&amount=100".to_string();

        let (status, _fields) = drive_form_post(session_token, body).await;

        assert_eq!(
            status, 419,
            "form POST with no _token (and no header) must reject with 419"
        );
    }

    #[tokio::test]
    async fn get_request_attaches_xsrf_token_cookie() {
        // Laravel's `tap()` after `isReading()` attaches the
        // XSRF-TOKEN cookie even on read requests — that's how SPA
        // clients ever acquire the cookie. Suprnova mirrors this:
        // every response carries Set-Cookie: XSRF-TOKEN=...
        let mw = Arc::new(CsrfMiddleware::new());
        let builder = hyper::Request::builder()
            .method("GET")
            .uri("http://localhost/");
        let driven = drive_request(mw, "expected-xsrf-token-abc", builder, None).await;
        assert_eq!(driven.status, 200);
        let cookies: Vec<&str> = driven
            .headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert!(
            cookies
                .iter()
                .any(|c| c.starts_with("XSRF-TOKEN=expected-xsrf-token-abc")),
            "expected XSRF-TOKEN cookie in {cookies:?}"
        );
    }

    #[tokio::test]
    async fn xsrf_cookie_is_not_http_only_so_js_can_read_it() {
        // Without HttpOnly=false the XSRF-TOKEN cookie is useless to
        // the SPA — that's the whole point of the convention. Pin
        // the absence of HttpOnly explicitly so a future "tighten
        // defaults" change can't silently break Axios / Angular.
        let mw = Arc::new(CsrfMiddleware::new());
        let builder = hyper::Request::builder()
            .method("GET")
            .uri("http://localhost/");
        let driven = drive_request(mw, "tok", builder, None).await;
        let cookies: Vec<String> = driven
            .headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok().map(str::to_owned))
            .collect();
        let xsrf = cookies
            .iter()
            .find(|c| c.starts_with("XSRF-TOKEN="))
            .expect("XSRF-TOKEN cookie present");
        assert!(
            !xsrf.contains("HttpOnly"),
            "XSRF-TOKEN must not be HttpOnly so JavaScript can echo it: {xsrf}"
        );
    }

    #[tokio::test]
    async fn without_xsrf_cookie_drops_the_cookie() {
        let mw = Arc::new(CsrfMiddleware::new().without_xsrf_cookie());
        let builder = hyper::Request::builder()
            .method("GET")
            .uri("http://localhost/");
        let driven = drive_request(mw, "tok", builder, None).await;
        let has_xsrf = driven
            .headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|c| c.starts_with("XSRF-TOKEN="));
        assert!(!has_xsrf, "XSRF cookie suppressed by builder");
    }

    #[tokio::test]
    async fn x_xsrf_token_header_round_trip_passes() {
        // Mirrors the full Laravel SPA flow: GET issues the
        // XSRF-TOKEN cookie → client echoes the cookie value in
        // X-XSRF-TOKEN on the next state-changing request → POST
        // passes without a form body at all.
        let token = "spa-round-trip-token-xyz";
        let mw = Arc::new(CsrfMiddleware::new());
        let builder = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost/api/transfer")
            .header("content-type", "application/json")
            .header("X-XSRF-TOKEN", token);
        let driven = drive_request(mw, token, builder, Some("{}".to_string())).await;
        assert_eq!(driven.status, 200);
    }

    #[tokio::test]
    async fn same_origin_sec_fetch_site_passes_under_allow_same_site_policy() {
        // `same-origin` always passes under any non-Disabled policy.
        let mw = Arc::new(CsrfMiddleware::new().allow_same_site());
        let builder = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost/api/post")
            .header("Sec-Fetch-Site", "same-origin");
        let driven = drive_request(mw, "tok", builder, None).await;
        assert_eq!(driven.status, 200);
    }

    #[tokio::test]
    async fn same_site_passes_only_under_allow_same_site_policy() {
        // SameOriginOnly (Laravel's default): `same-site` does NOT
        // short-circuit; must still present a token. With no token,
        // falls through and rejects 419.
        let mw = Arc::new(CsrfMiddleware::new().with_origin_policy(OriginPolicy::SameOriginOnly));
        let builder = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost/api/post")
            .header("Sec-Fetch-Site", "same-site");
        let driven = drive_request(mw, "tok", builder, None).await;
        assert_eq!(
            driven.status, 419,
            "same-site is not enough under default policy"
        );

        // AllowSameSite: `same-site` is now sufficient.
        let mw = Arc::new(CsrfMiddleware::new().allow_same_site());
        let builder = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost/api/post")
            .header("Sec-Fetch-Site", "same-site");
        let driven = drive_request(mw, "tok", builder, None).await;
        assert_eq!(driven.status, 200);
    }

    #[tokio::test]
    async fn cross_site_falls_through_to_token_check_under_non_origin_only_policy() {
        // `cross-site` doesn't pass origin. Without a token, falls
        // through to 419 rather than 403 — only OriginOnly mode
        // converts an origin failure into a 403.
        let mw = Arc::new(CsrfMiddleware::new().allow_same_site());
        let builder = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost/api/post")
            .header("Sec-Fetch-Site", "cross-site");
        let driven = drive_request(mw, "tok", builder, None).await;
        assert_eq!(driven.status, 419);
    }

    #[tokio::test]
    async fn origin_only_rejects_cross_site_with_403() {
        // Origin-only mode: an origin mismatch is fatal and the
        // status is 403 (Laravel's OriginMismatchException), not 419.
        let mw = Arc::new(CsrfMiddleware::new().origin_only());
        let builder = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost/api/post")
            .header("Sec-Fetch-Site", "cross-site");
        let driven = drive_request(mw, "tok", builder, None).await;
        assert_eq!(driven.status, 403);
    }

    #[tokio::test]
    async fn origin_only_rejects_missing_header_with_403() {
        // No Sec-Fetch-Site at all under OriginOnly = 403. This is
        // the documented HTTP-only caveat: a non-HTTPS server can't
        // use origin-only mode because browsers won't emit the
        // header.
        let mw = Arc::new(CsrfMiddleware::new().origin_only());
        let builder = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost/api/post");
        let driven = drive_request(mw, "tok", builder, None).await;
        assert_eq!(driven.status, 403);
    }

    #[tokio::test]
    async fn origin_only_does_not_attach_xsrf_cookie() {
        // Laravel: `shouldAddXsrfTokenCookie` returns false when
        // originOnly is on — no point shipping a token nobody uses.
        let mw = Arc::new(CsrfMiddleware::new().origin_only());
        let builder = hyper::Request::builder()
            .method("GET")
            .uri("http://localhost/");
        let driven = drive_request(mw, "tok", builder, None).await;
        // GET passes the reading-verb fast path → 200.
        assert_eq!(driven.status, 200);
        let cookies: Vec<&str> = driven
            .headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert!(
            !cookies.iter().any(|c| c.starts_with("XSRF-TOKEN=")),
            "no XSRF cookie under originOnly: {cookies:?}"
        );
    }

    #[tokio::test]
    async fn no_session_returns_419_not_500() {
        // Regression for MODULE_REVIEW_NOTES "Medium: missing
        // session context returns a 500". Matches Laravel:
        // `tokensMatch()` returning false is a 419, not a server
        // error. We simulate "no session" by leaving the slot empty
        // inside SESSION_CONTEXT before the middleware runs.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let io = TokioIo::new(stream);
            let service = service_fn(
                move |hyper_req: hyper::Request<hyper::body::Incoming>| async move {
                    // Empty session slot — get_csrf_token() returns None.
                    let slot = Arc::new(Mutex::new(None));
                    let response = SESSION_CONTEXT
                        .scope(slot, async move {
                            let req = Request::new(hyper_req);
                            let mw = Arc::new(CsrfMiddleware::new());
                            let next: Next = Arc::new(move |_req| {
                                Box::pin(async move { Ok(HttpResponse::text("should not reach")) })
                            });
                            mw.handle(req, next).await
                        })
                        .await;
                    let http = response.unwrap_or_else(|e| e);
                    Ok::<_, Infallible>(http.into_hyper())
                },
            );
            let _ = http1::Builder::new().serve_connection(io, service).await;
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let io = TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let req = hyper::Request::builder()
            .method("POST")
            .uri("http://localhost/login")
            .body(Full::new(Bytes::new()))
            .unwrap();
        let resp = sender.send_request(req).await.unwrap();
        assert_eq!(
            resp.status().as_u16(),
            419,
            "missing session token is a 419 (token mismatch), not a 500"
        );
    }

    // Silence unused-import warnings when only some of these are used.
    #[allow(dead_code)]
    fn _unused_imports_keep() {
        let _ = Empty::<Bytes>::new();
    }
}
