//! Phase 11 dogfood — email verification.
//!
//! Two handlers:
//!
//! - `POST /auth/verify/resend?email=alice@…` — request a fresh
//!   verification link. Anti-enumeration: always responds 200 with
//!   the same body whether or not the email is on file. The mail
//!   dispatch is owned by `EmailVerification::resend`, which looks the
//!   user up through the configured provider and only mints + sends a
//!   token when an account exists (so probing cannot differentiate via
//!   status code or payload).
//! - `GET  /auth/verify?token=…` — consume the token via the
//!   `EmailVerification` facade and 302 back to `/`.
//!
//! Both handlers split the body into a thin public wrapper +
//! `_inner` returning `Result<HttpResponse, FrameworkError>` so the
//! `?` operator works on framework errors without re-wrapping every
//! call site — same pattern as `controllers::posts`.

use std::collections::HashMap;

use suprnova::auth_flows::EmailVerification;
use suprnova::{FrameworkError, HttpResponse, Request, Response};

/// `POST /auth/verify/resend?email=...` — anti-enumeration resend.
///
/// Always returns 200 with the same body. The actual lookup + dispatch
/// is owned by `EmailVerification::resend`, which resolves the user
/// through the configured provider and only mints + sends a verification
/// link when an account is on file. An unknown email is a silent no-op
/// (still `Ok`), so an attacker probing the endpoint cannot distinguish
/// the two branches through status code or response payload.
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

    let base = format!(
        "{}/auth/verify",
        std::env::var("APP_URL").unwrap_or_else(|_| "http://localhost:8080".into())
    );
    // `resend` is anti-enumeration: it sends only when the email is on
    // file and returns `Ok(())` either way, so both branches respond
    // identically below.
    EmailVerification::resend(email, &base).await?;

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
/// expired tokens propagate as a `400 Bad Request` from the facade.
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
    Ok(HttpResponse::new().status(302).header("Location", "/"))
}
