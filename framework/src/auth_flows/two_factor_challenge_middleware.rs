//! [`TwoFactorChallengeMiddleware`] — gate routes when the session
//! has a 2FA challenge pending.
//!
//! Composes in front of [`crate::AuthMiddleware`]: this middleware
//! checks for a *pending* 2FA challenge (set by
//! [`crate::auth_flows::TwoFactor::start_challenge`]) and short-
//! circuits the request before `AuthMiddleware` sees the missing
//! `Auth::id()` and bounces the user to the login page. The natural
//! order:
//!
//! ```text
//! TwoFactorChallengeMiddleware  (pending → challenge)
//!     ↓
//! AuthMiddleware                (no auth at all → login)
//!     ↓
//! protected handler             (fully authenticated)
//! ```
//!
//! Routes that ARE the challenge page itself (GET / POST the form)
//! must NOT install this middleware — they are the destination. They
//! typically install no middleware at all and let the handler check
//! [`crate::auth_flows::TwoFactor::pending_user_id`] up front.

use async_trait::async_trait;

use crate::auth_flows::TwoFactor;
use crate::http::{HttpResponse, Request, Response};
use crate::middleware::{Middleware, Next};

/// Middleware that 302s (or 403s) any request whose session has a
/// 2FA challenge pending — the user authenticated their password but
/// has not yet completed the TOTP challenge.
///
/// The choice between **403 JSON** and **302 HTML redirect** is made
/// at route-registration time via the constructor, matching the
/// pattern set by [`crate::AuthMiddleware::new`] /
/// [`crate::AuthMiddleware::redirect_to`] and
/// [`crate::EnsureEmailVerifiedMiddleware`].
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{AuthMiddleware, TwoFactorChallengeMiddleware, group, get};
///
/// // API surface — 403 JSON until the challenge completes
/// group!("/api")
///     .middleware(TwoFactorChallengeMiddleware::new())
///     .middleware(AuthMiddleware::new())
///     .routes([
///         get!("/me", profile::show),
///     ]);
///
/// // Web surface — 302 to the challenge form
/// group!("/dashboard")
///     .middleware(TwoFactorChallengeMiddleware::redirect_to("/two-factor-challenge"))
///     .middleware(AuthMiddleware::redirect_to("/login"))
///     .routes([
///         get!("/", dashboard::index),
///     ]);
/// ```
///
/// # Pass-through cases
///
/// - Fully authenticated user (no pending challenge) → `next(request)`.
/// - No auth state at all (no pending, no authed) → `next(request)`,
///   so the downstream `AuthMiddleware` handles the unauthed case.
///
/// # Inertia
///
/// When the `redirect_to` form is used and the request is detected
/// as an Inertia visit, the response is `409 Conflict` with an
/// `X-Inertia-Location` header — Inertia performs a full-page visit
/// to the target. Plain HTML redirects use `302 Found` with a
/// `Location` header.
pub struct TwoFactorChallengeMiddleware {
    /// Path to redirect pending users to. `None` → respond with
    /// `403` JSON instead.
    redirect_to: Option<String>,
}

impl TwoFactorChallengeMiddleware {
    /// Create middleware that returns `403 Forbidden` with a JSON
    /// body `{"message": "Two-factor authentication challenge
    /// pending; complete it before continuing."}` when the
    /// authenticated user has not yet cleared the 2FA challenge.
    /// Best for API routes.
    pub fn new() -> Self {
        Self { redirect_to: None }
    }

    /// Create middleware that redirects pending users to `path`.
    /// Best for web routes — pair with
    /// [`crate::AuthMiddleware::redirect_to`]. Inertia requests
    /// receive `409 Conflict` + `X-Inertia-Location` instead of
    /// `302`.
    pub fn redirect_to(path: impl Into<String>) -> Self {
        Self {
            redirect_to: Some(path.into()),
        }
    }

    /// Build the "challenge pending" response — either a redirect or
    /// a `403` JSON, depending on how the middleware was constructed.
    fn challenge_response(&self, request: &Request) -> HttpResponse {
        match &self.redirect_to {
            Some(path) => {
                if request.is_inertia() {
                    HttpResponse::text("")
                        .status(409)
                        .header("X-Inertia-Location", path.clone())
                } else {
                    HttpResponse::new()
                        .status(302)
                        .header("Location", path.clone())
                }
            }
            None => HttpResponse::json(serde_json::json!({
                "message": "Two-factor authentication challenge pending; complete it before continuing."
            }))
            .status(403),
        }
    }
}

impl Default for TwoFactorChallengeMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for TwoFactorChallengeMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        if TwoFactor::pending_user_id().is_some() {
            // Pending: short-circuit before the request reaches a
            // protected handler.
            Err(self.challenge_response(&request))
        } else {
            // Not pending — could be fully authed, could be a guest.
            // Pass through; downstream `AuthMiddleware` handles the
            // guest case.
            next(request).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_no_redirect() {
        let mw = TwoFactorChallengeMiddleware::new();
        assert!(mw.redirect_to.is_none());
    }

    #[test]
    fn redirect_to_stores_path() {
        let mw = TwoFactorChallengeMiddleware::redirect_to("/two-factor-challenge");
        assert_eq!(mw.redirect_to.as_deref(), Some("/two-factor-challenge"));
    }

    #[test]
    fn default_is_no_redirect() {
        let mw = TwoFactorChallengeMiddleware::default();
        assert!(mw.redirect_to.is_none());
    }
}
