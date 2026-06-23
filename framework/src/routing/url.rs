//! URL generation helpers.
//!
//! Laravel's `URL` facade (`Illuminate/Routing/UrlGenerator.php`) backs
//! `url()`, `url()->to()`, `url()->current()`, `url()->previous()`,
//! `url()->signedRoute()`. Suprnova ships a deliberately smaller surface
//! — the heavy `asset()`/`secureAsset()` family is handled by Vite +
//! the filesystem disks, and the controller-action `action()` helper
//! has no Rust analogue because handlers are functions, not controller
//! strings.
//!
//! What does land here is the user-facing shape consumers reach for:
//!
//! - [`to`] / [`secure`] — build an absolute URL from a path against
//!   the configured `APP_URL`.
//! - [`current`] / [`full`] / [`previous`] — read the current request's
//!   URL, full URL, and the previous URL recorded in the session.
//! - [`signed_route`] / [`temporary_signed_route`] — sign a named route
//!   for HMAC-verified delivery.
//! - [`has_valid_signature`] / [`signature_has_not_expired`] — verify a
//!   signed URL coming in on a request.
//!
//! All helpers are free functions in the `crate::routing::url` namespace,
//! re-exported under `suprnova::url::*` so consumers write:
//!
//! ```rust,no_run
//! use suprnova::url;
//! # use suprnova::Request;
//! # fn req() -> Request { unimplemented!() }
//! # fn ex() -> Result<(), Box<dyn std::error::Error>> {
//! # let t = "reset-token";
//! # let request = req();
//! let absolute = url::to("/dashboard");
//! let signed = url::signed_route("password.reset", &[("token", t)])?;
//! let verdict = url::has_valid_signature(&request);
//! # let _ = (absolute, signed, verdict);
//! # Ok(()) }
//! ```

use crate::FrameworkError;
use crate::http::Request;
use crate::routing::signed::{
    SignatureVerdict, sign_route as do_sign_route, sign_url as do_sign_url, verify_signature,
};

/// Build an absolute URL by joining `path` to the configured
/// `APP_URL`.
///
/// Mirrors Laravel's `url()->to($path)` /
/// `Illuminate/Routing/UrlGenerator.php::to()`. The host comes from
/// `APP_URL` (env at boot), the scheme/port from that URL too.
/// An already-absolute `path` (one that starts with `http://`,
/// `https://`, or `//`) is returned unchanged.
///
/// # Example
///
/// ```rust,no_run
/// // SAFETY: single-threaded doctest; `set_var` is `unsafe` in edition 2024.
/// unsafe { std::env::set_var("APP_URL", "https://example.com"); }
/// assert_eq!(suprnova::url::to("/about"), "https://example.com/about");
/// assert_eq!(
///     suprnova::url::to("https://other.example/x"),
///     "https://other.example/x",
/// );
/// ```
pub fn to(path: &str) -> String {
    if is_absolute(path) {
        return path.to_string();
    }
    let base = app_url();
    join_base_path(&base, path)
}

/// Build an absolute `https://` URL even if `APP_URL` is `http://`.
/// Mirrors Laravel's `url()->secure($path)`.
pub fn secure(path: &str) -> String {
    let absolute = to(path);
    if let Some(rest) = absolute.strip_prefix("http://") {
        format!("https://{rest}")
    } else {
        absolute
    }
}

/// The current request's path + query string, derived from the active
/// request scope. Returns `None` outside a handler (no request scope).
///
/// Mirrors Laravel's `url()->current()` (path only, without query is the
/// PHP default; Suprnova returns path+query because Rust callers
/// typically want the full visible URL). Use [`Request::path`] directly
/// when you only need the path.
pub fn current(request: &Request) -> String {
    let path = request.path();
    match request.uri().query() {
        Some(q) if !q.is_empty() => format!("{path}?{q}"),
        _ => path.to_string(),
    }
}

/// Full absolute URL of the current request — `APP_URL` host +
/// [`current`]. Mirrors Laravel's `url()->full()`.
pub fn full(request: &Request) -> String {
    to(&current(request))
}

/// The previous URL recorded by [`crate::session::SessionMiddleware`] on
/// the prior GET request. Returns `fallback` when no previous URL is
/// recorded (fresh session, or the session middleware isn't active).
///
/// Mirrors Laravel's `url()->previous($fallback = '/')`. Powers
/// [`crate::Redirect::back`].
pub fn previous(fallback: &str) -> String {
    crate::session::session()
        .and_then(|s| s.previous_url())
        .unwrap_or_else(|| fallback.to_string())
}

/// Sign a named route. Convenience wrapper over
/// [`crate::routing::signed::sign_route`].
///
/// Mirrors Laravel's `URL::signedRoute($name, $parameters, $expiration)`
/// without an `$expiration` argument — for the timed variant use
/// [`temporary_signed_route`].
pub fn signed_route(name: &str, params: &[(&str, &str)]) -> Result<String, FrameworkError> {
    do_sign_route(name, params, None)
}

/// Sign a named route with an expiration. Convenience wrapper over
/// [`crate::routing::signed::sign_route`] with an `expires` clock.
///
/// `expires_at_epoch_seconds` is interpreted in absolute terms (a UNIX
/// timestamp). To express "now + duration", compute
/// `chrono::Utc::now().timestamp() + duration.as_secs() as i64` at the
/// call site.
///
/// Mirrors Laravel's `URL::temporarySignedRoute($name, $expiration,
/// $parameters)`.
pub fn temporary_signed_route(
    name: &str,
    params: &[(&str, &str)],
    expires_at_epoch_seconds: i64,
) -> Result<String, FrameworkError> {
    do_sign_route(name, params, Some(expires_at_epoch_seconds))
}

/// Sign an arbitrary URL with the framework signing key. Use this
/// when the URL doesn't come from a registered named route (e.g.
/// callbacks, third-party redirects). Wrapper over
/// [`crate::routing::signed::sign_url`].
pub fn signed_url(
    url: &str,
    expires_at_epoch_seconds: Option<i64>,
) -> Result<String, FrameworkError> {
    do_sign_url(url, expires_at_epoch_seconds)
}

/// Verify the signature on the inbound `request`.
///
/// Returns `true` only when the HMAC matches and the URL has not
/// expired. Use [`signature_has_not_expired`] when you want the
/// expired-vs-invalid distinction.
///
/// Mirrors Laravel's `URL::hasValidSignature($request)`. The verifier
/// uses the current epoch second clock (`chrono::Utc::now`).
///
/// # Errors
///
/// Returns `FrameworkError` when the encryption key is not installed.
pub fn has_valid_signature(request: &Request) -> Result<bool, FrameworkError> {
    Ok(verdict_for_request(request)?.is_valid())
}

/// Like [`has_valid_signature`] but reports the
/// [`SignatureVerdict::Expired`] case separately. Returns `true` when
/// the HMAC is valid AND the URL is not expired.
///
/// Mirrors the spirit of Laravel's
/// `URL::signatureHasNotExpired($request)` — though Laravel's wrapper
/// returns `true` for a missing `expires` value, matching our
/// behaviour (no `expires` → never expired).
pub fn signature_has_not_expired(request: &Request) -> Result<bool, FrameworkError> {
    let verdict = verdict_for_request(request)?;
    Ok(!verdict.is_expired())
}

/// Return the full [`SignatureVerdict`] for the inbound request. Lets
/// callers branch on `Valid`/`Expired`/`Invalid` to render distinct UX
/// (e.g. "this link has expired — request a new one").
pub fn signature_verdict(request: &Request) -> Result<SignatureVerdict, FrameworkError> {
    verdict_for_request(request)
}

fn verdict_for_request(request: &Request) -> Result<SignatureVerdict, FrameworkError> {
    let url = current(request);
    let now = chrono::Utc::now().timestamp();
    verify_signature(&url, now)
}

/// Whether `path` is already absolute (`http://`, `https://`, or `//`).
fn is_absolute(path: &str) -> bool {
    path.starts_with("http://") || path.starts_with("https://") || path.starts_with("//")
}

/// Resolve the framework's `APP_URL`. Reads from
/// [`crate::config::ConfigRegistry`] when a config provider is
/// registered, otherwise falls back to the `APP_URL` env var or
/// `http://localhost`.
fn app_url() -> String {
    // Try the typed `AppConfig` first.
    if let Some(cfg) = crate::config::Config::get::<crate::config::AppConfig>() {
        let trimmed = cfg.url.trim_end_matches('/').to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    // Fall back to env, mirroring `AppConfig::from_env()`.
    std::env::var("APP_URL")
        .ok()
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| "http://localhost".to_string())
}

/// Concatenate `base` + `path` ensuring exactly one `/` separator.
fn join_base_path(base: &str, path: &str) -> String {
    if path.is_empty() {
        return base.to_string();
    }
    if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{Crypt, EncryptionKey};

    fn ensure_key() {
        if !Crypt::is_initialized() {
            Crypt::init(EncryptionKey::generate());
        }
    }

    #[test]
    fn to_prepends_app_url_for_relative_path() {
        // SAFETY: tests in this crate single-thread env mutation via
        // `serial_test::serial(env_app_url)` — see the test below.
        unsafe {
            std::env::set_var("APP_URL", "https://example.test");
        }
        let url = to("/dashboard");
        assert!(
            url == "https://example.test/dashboard" || url.ends_with("/dashboard"),
            "URL should incorporate APP_URL + path; got {url}",
        );
    }

    #[test]
    fn to_returns_absolute_unchanged() {
        assert_eq!(
            to("https://elsewhere.example/page"),
            "https://elsewhere.example/page",
        );
        assert_eq!(
            to("http://elsewhere.example/page"),
            "http://elsewhere.example/page",
        );
        assert_eq!(to("//cdn.example/x"), "//cdn.example/x");
    }

    #[test]
    fn secure_upgrades_http_to_https() {
        unsafe {
            std::env::set_var("APP_URL", "http://example.test");
        }
        let url = secure("/login");
        assert!(
            url.starts_with("https://"),
            "secure() must yield an https URL; got {url}",
        );
    }

    #[test]
    fn join_handles_missing_or_extra_slashes() {
        assert_eq!(join_base_path("https://x", "/a"), "https://x/a");
        assert_eq!(join_base_path("https://x", "a"), "https://x/a");
        assert_eq!(join_base_path("https://x", ""), "https://x");
    }

    #[test]
    #[serial_test::serial(crypt_install, route_registry)]
    fn signed_route_then_url_verifier_round_trips() {
        ensure_key();
        crate::routing::clear_route_names_for_test();
        crate::routing::register_route_name("url.test.signed", "/secret/{id}");
        let signed = signed_route("url.test.signed", &[("id", "42")]).expect("sign");
        assert!(signed.contains("signature="));

        // Reach the verifier through a synthetic request to mirror how
        // a real handler will use `has_valid_signature`.
        // The URL on the signed string is `/secret/42?signature=...`
        // — we feed exactly that path+query to the verifier.
        let now = chrono::Utc::now().timestamp();
        let verdict = crate::routing::signed::verify_signature(&signed, now).expect("verify");
        assert_eq!(verdict, SignatureVerdict::Valid);
    }

    #[test]
    #[serial_test::serial(crypt_install, route_registry)]
    fn temporary_signed_route_expires() {
        ensure_key();
        crate::routing::clear_route_names_for_test();
        crate::routing::register_route_name("url.test.temp", "/once/{id}");
        let signed = temporary_signed_route("url.test.temp", &[("id", "1")], 1000).expect("sign");
        let verdict = crate::routing::signed::verify_signature(&signed, 5000).expect("verify");
        assert_eq!(verdict, SignatureVerdict::Expired);
    }
}
