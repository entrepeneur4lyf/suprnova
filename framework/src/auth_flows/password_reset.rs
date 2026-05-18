//! `PasswordReset` — facade over `Torii::password_reset()`.
//!
//! Wraps request / verify-without-consume / complete. [`PasswordReset::send_link`]
//! dispatches [`crate::auth_flows::PasswordResetMail`] via the
//! [`crate::Mail`] facade. [`PasswordReset::complete`] dispatches
//! [`crate::auth_flows::PasswordChangedMail`] as a fire-and-forget security
//! notification and fires
//! [`crate::auth_flows::events::PasswordResetCompleted`].
//!
//! # Why a facade
//!
//! Same rationale as [`crate::auth_flows::EmailVerification`]: application
//! code should never have to thread the `Torii<R>` generic, and the only
//! varying side-effect (the outbound emails) is delivered through
//! `Mail::to(...).send(...)`.
//!
//! # Anti-enumeration semantics
//!
//! Both [`PasswordReset::request`] and [`PasswordReset::send_link`] are
//! anti-enumeration: callers cannot distinguish "email exists" from
//! "email does not exist" through the return type or through whether
//! mail was dispatched.
//!
//! * `request` returns `Ok(None)` when the email is not on file (so
//!   no token is generated and no row is created).
//! * `send_link` always returns `Ok(())` — when the email is absent
//!   no mail is sent, and the absence is **not** leaked through an
//!   `Err`. Callers that need to distinguish for internal accounting
//!   should call [`PasswordReset::request`] directly.
//!
//! # Failure semantics on `complete()`
//!
//! Token consumption (the actual password update) commits inside torii
//! before the security-notification email is dispatched and before the
//! [`crate::auth_flows::events::PasswordResetCompleted`] event fires. A
//! mail-transport failure or a listener panic therefore cannot un-reset
//! the password. We log the mail failure via tracing and discard the
//! event-dispatch error — a side-effect on a notification path must
//! never roll back a successful reset.

use crate::auth_flows::mail::{PasswordChangedMail, PasswordResetMail};
use crate::error::FrameworkError;
use crate::mail::Mail;
use crate::torii_integration::{instance, User};

/// Facade for password-reset token operations.
///
/// All methods delegate to the global Torii instance — call
/// [`crate::torii_integration::init_torii`] before invoking any of them.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::auth_flows::PasswordReset;
///
/// // From the "forgot password" form:
/// PasswordReset::send_link(&email, "https://example.com/reset").await?;
///
/// // From the click-through handler, after the user enters a new password:
/// let user = PasswordReset::complete(&token_from_query, &new_password).await?;
/// ```
pub struct PasswordReset;

impl PasswordReset {
    /// Request a password-reset token. Returns `Some((user, plaintext_token))`
    /// when the email is on file, `Ok(None)` when it isn't — call sites that
    /// dispatch mail off the result must never leak the difference.
    ///
    /// Torii's default expiration is 15 minutes (per
    /// `PasswordResetService::request_password_reset`); use
    /// [`PasswordReset::request_with_expiration`] for a custom window.
    pub async fn request(email: &str) -> Result<Option<(User, String)>, FrameworkError> {
        instance()?
            .password_reset()
            .request(email)
            .await
            .map_err(map_err)
    }

    /// Request a password-reset token with a custom expiration window.
    /// Same anti-enumeration `Ok(None)` semantics as [`PasswordReset::request`].
    pub async fn request_with_expiration(
        email: &str,
        expires_in: chrono::Duration,
    ) -> Result<Option<(User, String)>, FrameworkError> {
        instance()?
            .password_reset()
            .request_with_expiration(email, expires_in)
            .await
            .map_err(map_err)
    }

    /// Check whether `token` is valid without consuming it.
    ///
    /// Useful for landing pages that want to confirm the token before
    /// rendering the new-password form, so a refresh does not burn the
    /// token.
    pub async fn verify_token(token: &str) -> Result<bool, FrameworkError> {
        instance()?
            .password_reset()
            .verify_token(token)
            .await
            .map_err(map_err)
    }

    /// Consume `token` and apply `new_password`. Returns the updated
    /// [`User`] on success.
    ///
    /// Side effects, in order:
    ///
    /// 1. The token row is consumed and the password hash is rotated
    ///    (transactionally, inside torii).
    /// 2. A [`PasswordChangedMail`] security notification is dispatched
    ///    through the [`Mail`] facade. A transport failure is logged via
    ///    `tracing::warn!` but does **not** surface as an `Err` — the
    ///    reset has already committed.
    /// 3. A [`crate::auth_flows::events::PasswordResetCompleted`] event
    ///    is fired. A dispatcher error is discarded (the dispatcher itself
    ///    logs listener errors via tracing).
    ///
    /// Reads `APP_NAME` (defaults to `"Suprnova"`) and `MAIL_FROM`
    /// (required — errors if unset) from the process environment.
    /// Defaulting `MAIL_FROM` to a placeholder breaks DMARC/SPF in
    /// production, so the facade fails closed instead of silently
    /// sending from a domain the operator doesn't control. If
    /// `MAIL_FROM` is unset the password change itself still commits
    /// — the failure surfaces from the notification path only.
    pub async fn complete(token: &str, new_password: &str) -> Result<User, FrameworkError> {
        let user = instance()?
            .password_reset()
            .complete(token, new_password)
            .await
            .map_err(map_err)?;

        // Revoke every session + remember-me row for this user. A
        // stolen session must not outlive the credential it depended
        // on; same for any persistent remember-me cookie. Both are
        // best-effort: failures log but do not roll back the
        // committed password change.
        let user_id_str = user.id.to_string();
        match crate::session::destroy_all_for_user(&user_id_str).await {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(
                        "revoked {n} session row(s) for user {user_id_str} after password reset"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "session revocation failed for user {user_id_str} after password reset: {e}"
                );
            }
        }
        match crate::auth::remember::revoke_all_for_user(&user_id_str).await {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(
                        "revoked {n} remember-me row(s) for user {user_id_str} after password reset"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "remember-me revocation failed for user {user_id_str} after password reset: {e}"
                );
            }
        }

        // Fire-and-forget security notification. A delivery failure here
        // must not roll back the already-committed password change.
        // require_mail_from() errors when MAIL_FROM is unset — same
        // fire-and-forget posture: log via tracing::warn, do not
        // surface to the caller.
        match crate::auth_flows::require_mail_from() {
            Ok(from_address) => {
                let to_address = user.email.clone();
                let mail = PasswordChangedMail {
                    to_address: to_address.clone(),
                    user_name: user.name.clone(),
                    app_name: crate::auth_flows::app_name(),
                    from_address,
                };
                if let Err(e) = Mail::to(to_address.as_str()).send(mail).await {
                    tracing::warn!(
                        "password-changed security notification failed for user {}: {e}",
                        user.id
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "password-changed security notification skipped for user {}: {e}",
                    user.id
                );
            }
        }

        // Intentionally discard the dispatch error — the reset has
        // already committed; a downstream listener failure must not
        // surface as a reset failure to the caller. The dispatcher
        // itself logs listener errors via tracing.
        let _ = crate::events::EventFacade::dispatch(
            crate::auth_flows::events::PasswordResetCompleted {
                user_id: user.id.to_string(),
            },
        )
        .await;

        Ok(user)
    }

    /// Generate a reset token (if the email is on file), build the reset URL,
    /// and dispatch [`PasswordResetMail`] via `Mail::to(&user.email).send(...)`.
    ///
    /// The URL has the shape `{base_url}?token={plaintext_token}`. A trailing
    /// slash on `base_url` is trimmed before the query string is appended.
    ///
    /// # Anti-enumeration
    ///
    /// Always returns `Ok(())`, regardless of whether the email is on file.
    /// When the email is absent **no mail is dispatched**, and the absence is
    /// not surfaced through the return type either. Callers that need to
    /// distinguish for internal accounting should call [`PasswordReset::request`]
    /// directly.
    ///
    /// Reads `APP_NAME` (defaults to `"Suprnova"`) and `MAIL_FROM`
    /// (required — errors if unset) from the process environment.
    /// Defaulting `MAIL_FROM` to a placeholder breaks DMARC/SPF in
    /// production, so the facade fails closed instead of silently
    /// sending from a domain the operator doesn't control.
    pub async fn send_link(email: &str, base_url: &str) -> Result<(), FrameworkError> {
        let Some((user, token)) = Self::request(email).await? else {
            // Anti-enumeration: silently succeed when the email is absent.
            return Ok(());
        };
        let url = format!("{}?token={}", base_url.trim_end_matches('/'), token);

        let to_address = user.email.clone();
        let mail = PasswordResetMail {
            to_address: to_address.clone(),
            user_name: user.name,
            reset_link: url,
            app_name: crate::auth_flows::app_name(),
            from_address: crate::auth_flows::require_mail_from()?,
        };

        Mail::to(to_address.as_str()).send(mail).await
    }
}

fn map_err(e: torii::ToriiError) -> FrameworkError {
    FrameworkError::internal(format!("torii password reset: {e}"))
}
