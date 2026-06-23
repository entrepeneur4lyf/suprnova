//! Bearer-token middleware for API-style authentication.
//!
//! Extracts an `Authorization: Bearer <token>` header from each request,
//! validates it against the Torii session store, and binds the resolved
//! `user_id` into the current per-request session scope so that
//! [`crate::Auth::check`] / [`crate::Auth::id`] work in downstream handlers.
//!
//! The `Bearer` scheme is matched case-insensitively per RFC 7235 §2.1, so
//! `Bearer`, `bearer`, `BEARER`, etc. all work. Scheme and credentials may
//! be separated by any number of SP/HTAB characters.
//!
//! # Behaviour
//!
//! - Header present **and** token valid **and** session exists → call
//!   [`crate::session::set_auth_user`] with the raw torii `UserId` string
//!   (e.g. `"usr_<base58>"`), then pass the request through.
//! - Header missing **or** token invalid **or** session not found → pass the
//!   request through unchanged. The middleware never returns `401`; that is
//!   [`crate::auth::AuthMiddleware`]'s responsibility.
//!
//! # User ID storage
//!
//! The raw torii `UserId` string is stored directly in the session's `user_id`
//! field. `Auth::id()` returns it as `Option<String>`, and
//! `Auth::user_as::<T>()` passes it to `UserProvider::retrieve_by_id(&str)`.
//! Applications that implement `UserProvider` receive the raw torii `UserId`
//! and can look up the user in their own store by that string.
//!
//! # Registration
//!
//! ```rust,no_run
//! use suprnova::{global_middleware, torii_integration::middleware::BearerTokenMiddleware};
//!
//! pub async fn register() {
//!     global_middleware!(BearerTokenMiddleware);
//! }
//! ```

use async_trait::async_trait;
use torii::SessionToken;

use crate::Request;
use crate::http::Response;
use crate::middleware::{Middleware, Next};
use crate::session::set_auth_user;

use super::instance;

/// Middleware that authenticates API requests via `Authorization: Bearer <token>`.
///
/// See [module documentation](self) for full behaviour.
pub struct BearerTokenMiddleware;

#[async_trait]
impl Middleware for BearerTokenMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Extract the Bearer token from the Authorization header.
        // hyper's HeaderMap uses case-insensitive keys, so this handles
        // both "Authorization" and "authorization".
        //
        // Per RFC 7235 §2.1, the auth-scheme is case-insensitive — accept
        // "Bearer", "bearer", "BEARER", "BeArEr", etc. Scheme and credentials
        // are separated by one-or-more SP/HTAB; we trim leading whitespace
        // on the credentials so any amount of whitespace works.
        if let Some(auth_header) = request.header("Authorization")
            && let Some(token_str) = strip_bearer_scheme(auth_header)
        {
            let token_str = token_str.trim();
            if !token_str.is_empty() {
                // Attempt to look up the session in the global Torii instance.
                // Any failure (Torii not initialised, token invalid, session
                // expired / not found) is treated as "no session" — pass through.
                if let Ok(torii) = instance() {
                    let session_token = SessionToken::from(token_str);
                    if let Ok(session) = torii.get_session(&session_token).await {
                        set_auth_user(session.user_id.as_str());
                    }
                }
            }
        }

        next(request).await
    }
}

/// Strip a case-insensitive `Bearer` scheme prefix from an `Authorization`
/// header value, returning the raw credentials (with any leading whitespace
/// still attached for the caller to trim).
///
/// Returns `None` if the header does not start with the `Bearer` scheme or
/// is not followed by whitespace (i.e. `Bearertoken` without a separator is
/// not a valid challenge per RFC 7235).
fn strip_bearer_scheme(header: &str) -> Option<&str> {
    const SCHEME: &str = "Bearer";

    if header.len() < SCHEME.len() {
        return None;
    }

    let (head, rest) = header.split_at(SCHEME.len());
    if !head.eq_ignore_ascii_case(SCHEME) {
        return None;
    }

    // RFC 7235: scheme and credentials must be separated by at least one
    // SP/HTAB. An empty `rest` (header is exactly "Bearer") or a `rest`
    // starting with a non-whitespace byte (e.g. "Bearertoken") is invalid.
    let first = rest.as_bytes().first()?;
    if !matches!(first, b' ' | b'\t') {
        return None;
    }

    Some(rest)
}

#[cfg(test)]
mod tests {
    use super::strip_bearer_scheme;

    #[test]
    fn strip_bearer_scheme_accepts_canonical_casing() {
        assert_eq!(strip_bearer_scheme("Bearer abc"), Some(" abc"));
    }

    #[test]
    fn strip_bearer_scheme_accepts_lowercase() {
        assert_eq!(strip_bearer_scheme("bearer abc"), Some(" abc"));
    }

    #[test]
    fn strip_bearer_scheme_accepts_uppercase() {
        assert_eq!(strip_bearer_scheme("BEARER abc"), Some(" abc"));
    }

    #[test]
    fn strip_bearer_scheme_accepts_mixed_casing() {
        assert_eq!(strip_bearer_scheme("BeArEr abc"), Some(" abc"));
    }

    #[test]
    fn strip_bearer_scheme_accepts_tab_separator() {
        assert_eq!(strip_bearer_scheme("Bearer\tabc"), Some("\tabc"));
    }

    #[test]
    fn strip_bearer_scheme_accepts_multiple_spaces() {
        assert_eq!(strip_bearer_scheme("Bearer   abc"), Some("   abc"));
    }

    #[test]
    fn strip_bearer_scheme_rejects_wrong_scheme() {
        assert_eq!(strip_bearer_scheme("Basic abc"), None);
    }

    #[test]
    fn strip_bearer_scheme_rejects_missing_separator() {
        // "Bearertoken" has no SP/HTAB between scheme and credentials.
        assert_eq!(strip_bearer_scheme("Bearertoken"), None);
    }

    #[test]
    fn strip_bearer_scheme_rejects_scheme_only() {
        assert_eq!(strip_bearer_scheme("Bearer"), None);
    }

    #[test]
    fn strip_bearer_scheme_rejects_empty() {
        assert_eq!(strip_bearer_scheme(""), None);
    }

    #[test]
    fn strip_bearer_scheme_rejects_too_short() {
        assert_eq!(strip_bearer_scheme("Be"), None);
    }
}
