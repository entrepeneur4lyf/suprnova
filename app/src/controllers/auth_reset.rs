//! Phase 11 dogfood — password reset.
//!
//! Two handlers:
//!
//! - `POST /auth/password/request` — body is form-urlencoded
//!   `email=...`. Calls `PasswordReset::send_link` (anti-enumeration:
//!   always returns 200, dispatches mail only when the email is on
//!   file — the facade itself enforces this).
//! - `POST /auth/password/reset` — body is form-urlencoded
//!   `token=...&new_password=...`. Calls `PasswordReset::complete`,
//!   redirects to `/?reset=ok` on success.
//!
//! Both endpoints are public — they consume tokens minted out-of-
//! band, so they don't sit behind `SessionAuthMiddleware`. Token
//! validity is the auth check.

use serde::Deserialize;
use suprnova::auth_flows::PasswordReset;
use suprnova::{FrameworkError, HttpResponse, Request, Response};

/// Body for `POST /auth/password/request`.
#[derive(Deserialize)]
pub struct RequestResetForm {
    pub email: String,
}

/// Body for `POST /auth/password/reset`.
#[derive(Deserialize)]
pub struct CompleteResetForm {
    pub token: String,
    pub new_password: String,
}

/// `POST /auth/password/request` — start a reset.
///
/// The `PasswordReset::send_link` facade is already anti-enumeration:
/// it returns `Ok(())` regardless of whether the email maps to a
/// real user, and only dispatches a `PasswordResetMail` when it
/// does. We don't need to add a second guard — surfacing the
/// facade's outcome directly is the right thing here.
pub async fn request_reset(req: Request) -> Response {
    request_reset_inner(req).await.map_err(HttpResponse::from)
}

async fn request_reset_inner(req: Request) -> Result<HttpResponse, FrameworkError> {
    let form: RequestResetForm = req.form().await?;

    let base = format!(
        "{}/auth/password/reset",
        std::env::var("APP_URL").unwrap_or_else(|_| "http://localhost:8080".into())
    );
    PasswordReset::send_link(&form.email, &base).await?;

    Ok(HttpResponse::text(
        "If this email is on file, a password reset link has been sent.",
    ))
}

/// `POST /auth/password/reset` — consume the token + apply the new
/// password.
///
/// On success the facade also dispatches a `PasswordChangedMail`
/// security notification and fires `PasswordResetCompleted`. Both
/// are fire-and-forget — a transport / listener failure does not
/// roll back the password rotation (matching the facade's documented
/// failure semantics).
pub async fn complete_reset(req: Request) -> Response {
    complete_reset_inner(req).await.map_err(HttpResponse::from)
}

async fn complete_reset_inner(req: Request) -> Result<HttpResponse, FrameworkError> {
    let form: CompleteResetForm = req.form().await?;

    if form.new_password.trim().is_empty() {
        return Err(FrameworkError::bad_request(
            "new_password must not be empty",
        ));
    }

    PasswordReset::complete(&form.token, &form.new_password).await?;

    // 302 → /?reset=ok . Built directly so the inner function's
    // `Result<HttpResponse, ...>` shape composes — see the matching
    // comment in `auth_verify::verify_inner` for the rationale.
    Ok(HttpResponse::new()
        .status(302)
        .header("Location", "/?reset=ok"))
}
