//! Phase 11 dogfood — TOTP two-factor authentication.
//!
//! All three handlers are session-gated by `SessionAuthMiddleware`
//! at the route layer (see `app/src/routes.rs`). Inside the handler
//! we still resolve `Auth::id()` defensively — if the middleware
//! is ever taken off the group by accident, we surface a 401 rather
//! than acting as some other user.
//!
//! - `POST /auth/2fa/enroll`  — generate secret + recovery codes,
//!   return otpauth URL + QR-code SVG + plaintext recovery codes.
//!   The recovery codes are shown exactly once; there is no later
//!   API to retrieve them.
//! - `POST /auth/2fa/confirm` — body `code=...`, stamp the
//!   enrollment as confirmed after the user proves they can read
//!   the TOTP code from their authenticator app.
//! - `POST /auth/2fa/disable` — delete the row + fire
//!   `TwoFactorDisabled` (only on real state transitions).
//!
//! The app's own `User` model is a stub with no `email` column, so
//! we look up the **torii** `User` by id — that record carries the
//! email field 2FA needs to render the authenticator-app label.

use serde::Deserialize;
use suprnova::auth_flows::{TwoFactor, TwoFactorUser};
use suprnova::torii_integration::{User as ToriiUser, find_user_by_id};
use suprnova::{Auth, FrameworkError, HttpResponse, Request, Response};

/// Body for `POST /auth/2fa/confirm` — form-urlencoded `code=...`.
#[derive(Deserialize)]
pub struct ConfirmForm {
    pub code: String,
}

/// Bridge from the framework's torii [`ToriiUser`] to the 2FA
/// facade's [`TwoFactorUser`] trait.
///
/// `TwoFactor::enroll` folds `email()` into the authenticator-app
/// label inside the otpauth URL — that's why the email is part of
/// the trait. `user_id()` is the opaque storage key the 2FA table
/// is indexed by; we use the torii `UserId` string so a future
/// migration that surfaces `Auth::user_as::<User>()` doesn't have
/// to rewrite the row keys.
struct AppUser2FA<'a> {
    user: &'a ToriiUser,
}

impl<'a> TwoFactorUser for AppUser2FA<'a> {
    fn user_id(&self) -> &str {
        self.user.id.as_str()
    }

    fn email(&self) -> &str {
        &self.user.email
    }
}

/// Resolve the current session's torii user, or fail 401.
///
/// The route group already sits behind `SessionAuthMiddleware`, so
/// in production `Auth::id()` is guaranteed `Some`. We still check
/// — the cheap defensive branch keeps these handlers honest if the
/// middleware ever gets disabled by accident.
async fn current_torii_user() -> Result<ToriiUser, FrameworkError> {
    let user_id = Auth::id().ok_or(FrameworkError::Unauthorized)?;
    find_user_by_id(&user_id)
        .await?
        .ok_or(FrameworkError::Unauthorized)
}

/// `POST /auth/2fa/enroll` — start enrollment.
///
/// Generates a fresh TOTP secret + 10 recovery codes, persists them
/// encrypted, and returns:
///
/// - `otpauth_url` — `otpauth://totp/...`, deep-linkable into any
///   authenticator app.
/// - `qr_code_svg` — SVG wrapping a base64 PNG; safe to embed inline.
/// - `recovery_codes` — ten plaintext single-use codes. **Show these
///   to the user exactly once** — there is no later retrieval API.
///
/// Until the user submits a valid code through
/// [`confirm`], 2FA is **not** enforced on the
/// account (`TwoFactor::is_enabled` returns `false`, `verify`
/// short-circuits to `false`).
pub async fn enroll(_req: Request) -> Response {
    enroll_inner().await.map_err(HttpResponse::from)
}

async fn enroll_inner() -> Result<HttpResponse, FrameworkError> {
    let user = current_torii_user().await?;
    let response = TwoFactor::enroll(&AppUser2FA { user: &user }).await?;

    Ok(HttpResponse::json(suprnova::serde_json::json!({
        "otpauth_url": response.otpauth_url,
        "qr_code_svg": response.qr_code_svg,
        "recovery_codes": response.recovery_codes,
    })))
}

/// `POST /auth/2fa/confirm` — confirm a pending enrollment.
///
/// Body is form-urlencoded `code=NNNNNN`. On success, stamps
/// `confirmed_at` on the row and fires `TwoFactorEnrolled`. An
/// invalid code surfaces as a 401 via the facade's domain error.
pub async fn confirm(req: Request) -> Response {
    confirm_inner(req).await.map_err(HttpResponse::from)
}

async fn confirm_inner(req: Request) -> Result<HttpResponse, FrameworkError> {
    // Snapshot the auth identity *before* consuming the request body
    // — `req.form()` takes `self`. Tying the read of `Auth::id()` to
    // the same task-local scope as the form parse keeps the
    // ordering deterministic for tests that mock session state.
    let user = current_torii_user().await?;
    let form: ConfirmForm = req.form().await?;

    TwoFactor::confirm(&AppUser2FA { user: &user }, &form.code).await?;

    Ok(HttpResponse::json(suprnova::serde_json::json!({
        "status": "confirmed",
    })))
}

/// `POST /auth/2fa/disable` — turn 2FA off for the current user.
///
/// Idempotent: a disable on an account that never enrolled is not
/// an error. Fires `TwoFactorDisabled` only on a real state
/// transition (matches the facade's documented contract).
pub async fn disable(_req: Request) -> Response {
    disable_inner().await.map_err(HttpResponse::from)
}

async fn disable_inner() -> Result<HttpResponse, FrameworkError> {
    let user = current_torii_user().await?;
    TwoFactor::disable(&AppUser2FA { user: &user }).await?;

    Ok(HttpResponse::json(suprnova::serde_json::json!({
        "status": "disabled",
    })))
}
