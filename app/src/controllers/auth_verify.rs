//! Phase 11 dogfood — email verification.
//!
//! Two handlers:
//!
//! - `POST /auth/verify/resend?email=alice@…` — request a fresh
//!   verification link. Anti-enumeration: always responds 200 with
//!   the same body whether or not the email is on file. The mail
//!   dispatch is conditional on `find_user_by_email_lookup_only`
//!   returning `Some` (so failed-attempt probing cannot create
//!   accounts or differentiate via response timing of a token mint).
//! - `GET  /auth/verify?token=…` — consume the token via the
//!   `EmailVerification` facade and 302 back to `/`.
//!
//! Both handlers split the body into a thin public wrapper +
//! `_inner` returning `Result<HttpResponse, FrameworkError>` so the
//! `?` operator works on framework errors without re-wrapping every
//! call site — same pattern as `controllers::posts`.

use std::collections::HashMap;

use suprnova::auth_flows::EmailVerification;
use suprnova::torii_integration::find_user_by_email_lookup_only;
use suprnova::{FrameworkError, HttpResponse, Request, Response};

/// `POST /auth/verify/resend?email=...` — anti-enumeration resend.
///
/// Always returns 200 with the same body. When the email maps to a
/// real user, dispatches a fresh verification link through
/// `EmailVerification::send_link`. When it doesn't, the handler
/// silently no-ops and returns the same body — an attacker probing
/// the endpoint cannot distinguish the two branches through status
/// code or response payload.
pub async fn resend(req: Request) -> Response {
    resend_inner(req).await.map_err(HttpResponse::from)
}

async fn resend_inner(req: Request) -> Result<HttpResponse, FrameworkError> {
    let raw = req.query().unwrap_or("");
    let params: HashMap<String, String> = url::form_urlencoded::parse(raw.as_bytes())
        .into_owned()
        .collect();
    let email = params
        .get("email")
        .ok_or_else(|| FrameworkError::bad_request("missing email query parameter"))?;

    // Anti-enumeration: only dispatch when the user exists; respond
    // identically in both branches. The lookup helper deliberately
    // never creates a row (`find_user_by_email_lookup_only`) so a
    // probing caller cannot mint accounts here either.
    if let Some(user) = find_user_by_email_lookup_only(email).await? {
        let base = format!(
            "{}/auth/verify",
            std::env::var("APP_URL").unwrap_or_else(|_| "http://localhost:8000".into())
        );
        EmailVerification::send_link(&user, &base).await?;
    }

    Ok(HttpResponse::text(
        "If this email is on file, a verification link has been sent.",
    ))
}

/// `GET /auth/verify?token=...` — consume a verification token.
///
/// Delegates the actual mutation to `EmailVerification::verify` (which
/// also fires the `EmailVerified` event). On success, 302s the user
/// back to `/`. On failure, the framework's standard error mapping
/// turns the `FrameworkError` into the right HTTP status — invalid /
/// expired tokens propagate as torii's domain error.
pub async fn verify(req: Request) -> Response {
    verify_inner(req).await.map_err(HttpResponse::from)
}

async fn verify_inner(req: Request) -> Result<HttpResponse, FrameworkError> {
    let raw = req.query().unwrap_or("");
    let params: HashMap<String, String> = url::form_urlencoded::parse(raw.as_bytes())
        .into_owned()
        .collect();
    let token = params
        .get("token")
        .ok_or_else(|| FrameworkError::bad_request("missing token query parameter"))?;

    EmailVerification::verify(token).await?;

    // 302 → / . Built directly so the `Result<HttpResponse, ...>`
    // shape of the inner function composes cleanly (the `From<Redirect>`
    // impl produces a `Response = Result<HttpResponse, HttpResponse>`,
    // not what we need here).
    Ok(HttpResponse::new()
        .status(302)
        .header("Location", "/"))
}
