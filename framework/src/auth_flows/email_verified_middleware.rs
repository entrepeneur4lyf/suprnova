//! [`EnsureEmailVerifiedMiddleware`] — gate routes on the authenticated
//! user's email-verification state.
//!
//! Mirrors Laravel's `Illuminate\Auth\Middleware\EnsureEmailIsVerified`
//! (the `verified` route alias). Composes naturally after
//! [`crate::AuthMiddleware`]: this middleware does not authenticate, it
//! only checks the verification flag on the user that auth already
//! resolved. The check goes through the application's configured
//! [`UserProvider`](crate::auth::UserProvider) — the same provider
//! [`Auth::user`](crate::auth::Auth::user) resolves against — so it is
//! backend-agnostic (Eloquent today; any registered provider tomorrow)
//! and carries no coupling to a specific auth store. If no user is
//! currently authenticated, it falls into the same response branch as
//! "user authed but not verified" — matching Laravel's
//! `! $request->user() || ! hasVerifiedEmail()` shape.

use async_trait::async_trait;

use crate::auth::{Auth, active_user_provider};
use crate::http::{HttpResponse, Request, Response};
use crate::middleware::{Middleware, Next};

/// Middleware that 403s (or redirects) any request whose authenticated
/// user has not verified their email.
///
/// The choice between **JSON-403** and **HTML-302-redirect** is made at
/// route-registration time via the constructor — there is no
/// request-content sniffing. This matches the pattern set by
/// [`crate::AuthMiddleware::new`] / [`crate::AuthMiddleware::redirect_to`].
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{AuthMiddleware, EnsureEmailVerifiedMiddleware, group};
///
/// // API routes — 403 JSON when unverified
/// group!("/api")
///     .middleware(AuthMiddleware::new())
///     .middleware(EnsureEmailVerifiedMiddleware::new())
///     .routes([/* ... */]);
///
/// // Web routes — 302 redirect to the "please verify your email" page
/// group!("/dashboard")
///     .middleware(AuthMiddleware::redirect_to("/login"))
///     .middleware(EnsureEmailVerifiedMiddleware::redirect_to("/email/verify"))
///     .routes([/* ... */]);
/// ```
///
/// # Inertia
///
/// When the `redirect_to` form is used and the request is detected as
/// an Inertia visit, the response is `409 Conflict` with an
/// `X-Inertia-Location` header — the Inertia adapter then performs a
/// full-page visit to the target. Plain HTML redirects use `302
/// Found` with a `Location` header. Matches the pattern in
/// [`crate::AuthMiddleware`].
pub struct EnsureEmailVerifiedMiddleware {
    /// Path to redirect unverified users to. `None` → respond with
    /// `403` JSON instead.
    redirect_to: Option<String>,
}

impl EnsureEmailVerifiedMiddleware {
    /// Create middleware that returns `403 Forbidden` with a JSON body
    /// `{"message": "Your email address is not verified."}` when the
    /// authenticated user has not verified their email (or when no
    /// user is authenticated at all).
    ///
    /// Best for API routes — pair with [`crate::AuthMiddleware::new`].
    pub fn new() -> Self {
        Self { redirect_to: None }
    }

    /// Create middleware that redirects unverified users to `path`.
    ///
    /// Best for web routes — pair with
    /// [`crate::AuthMiddleware::redirect_to`]. Inertia requests
    /// receive `409 Conflict` + `X-Inertia-Location` instead of `302`.
    pub fn redirect_to(path: impl Into<String>) -> Self {
        Self {
            redirect_to: Some(path.into()),
        }
    }

    /// Build the "not verified" response — either a redirect or a
    /// `403` JSON, depending on how the middleware was constructed.
    fn unverified_response(&self, request: &Request) -> HttpResponse {
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
                "message": "Your email address is not verified."
            }))
            .status(403),
        }
    }
}

impl Default for EnsureEmailVerifiedMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Middleware for EnsureEmailVerifiedMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // 1. Pull the auth id from the request state (sync; no DB call).
        let Some(user_id) = Auth::id() else {
            // No authenticated user — same response branch as "authed
            // but unverified" (mirrors Laravel's `! user() || ! verified`).
            return Err(self.unverified_response(&request));
        };

        // 2. Ask the application's configured `UserProvider` whether this
        //    user has verified their email. A `?` here propagates a
        //    `FrameworkError` — e.g. the storage layer is down, or the
        //    active provider is token-only and doesn't support the check
        //    (its default impl returns an unsupported error) — as the
        //    framework's usual 500. That's the correct behaviour when the
        //    provider can't answer the question we were composed to ask:
        //    verification gating must not silently pass under outage or
        //    misconfiguration.
        //
        //    The Eloquent provider returns `Ok(false)` for an absent id
        //    (the user was deleted after auth resolved it), so a missing
        //    user collapses into the unverified branch below — preserving
        //    the prior "user since deleted → unverified" behaviour.
        let verified = active_user_provider()?.is_email_verified(&user_id).await?;

        if verified {
            next(request).await
        } else {
            Err(self.unverified_response(&request))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_no_redirect() {
        let mw = EnsureEmailVerifiedMiddleware::new();
        assert!(mw.redirect_to.is_none());
    }

    #[test]
    fn redirect_to_stores_path() {
        let mw = EnsureEmailVerifiedMiddleware::redirect_to("/verify");
        assert_eq!(mw.redirect_to.as_deref(), Some("/verify"));
    }

    #[test]
    fn default_is_no_redirect() {
        let mw = EnsureEmailVerifiedMiddleware::default();
        assert!(mw.redirect_to.is_none());
    }
}
