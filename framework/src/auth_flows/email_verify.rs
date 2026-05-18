//! `EmailVerification` — facade over `Torii::email_verification()`.
//!
//! Generates verification tokens, checks them, verifies (consumes) them,
//! and dispatches the verification email through Suprnova's [`crate::Mail`]
//! facade. Verification fires an [`EmailVerified`](crate::auth_flows::events::EmailVerified)
//! event so listeners can react (e.g. unlock additional functionality, send
//! a welcome email).
//!
//! # Why a facade
//!
//! Application code never has a reason to reach into torii directly for
//! email verification — every operation is uniform across drivers and
//! the only side-effect that varies (the outbound email) is delivered
//! through `Mail::to(...).send(...)`. Surfacing a single `EmailVerification`
//! type lets us hide the `Torii<R>` generic and keep consumer imports
//! pointed at `suprnova::*`.
//!
//! # Failure semantics on `verify()`
//!
//! Token consumption (the actual mutation of `email_verified_at`) happens
//! before the `EmailVerified` event fires. A listener panic or transient
//! event-dispatcher error therefore cannot un-verify the user. We log the
//! dispatch error via the dispatcher's own tracing instrumentation but
//! return `Ok(user)` regardless — a side-effect on a notification path
//! must never roll back a successful verification.

use crate::auth_flows::mail::EmailVerificationMail;
use crate::error::FrameworkError;
use crate::mail::Mail;
use crate::torii_integration::{instance, User, UserId};
use torii::SecureToken;

/// Facade for email-verification token operations.
///
/// All methods delegate to the global Torii instance — call
/// [`crate::torii_integration::init_torii`] before invoking any of them.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::auth_flows::EmailVerification;
///
/// // After a fresh signup:
/// EmailVerification::send_link(&user, "https://example.com/verify").await?;
///
/// // From the click-through handler:
/// let user = EmailVerification::verify(&token_from_query).await?;
/// ```
pub struct EmailVerification;

impl EmailVerification {
    /// Generate a verification token for `user_id` with torii's default
    /// expiration window (24h, per torii's `EmailVerificationService`).
    ///
    /// The returned [`SecureToken`] exposes the plaintext via
    /// [`SecureToken::token`] — store the hash, hand the plaintext to the
    /// user.
    pub async fn generate_token(user_id: &UserId) -> Result<SecureToken, FrameworkError> {
        instance()?
            .email_verification()
            .generate_token(user_id)
            .await
            .map_err(map_err)
    }

    /// Generate a verification token with a custom expiration window.
    pub async fn generate_token_with_expiration(
        user_id: &UserId,
        expires_in: chrono::Duration,
    ) -> Result<SecureToken, FrameworkError> {
        instance()?
            .email_verification()
            .generate_token_with_expiration(user_id, expires_in)
            .await
            .map_err(map_err)
    }

    /// Check whether `token` is valid without consuming it.
    ///
    /// Useful for landing pages that want to display a "click to verify"
    /// button before actually consuming the token (so a refresh does not
    /// burn the token).
    pub async fn check(token: &str) -> Result<bool, FrameworkError> {
        instance()?
            .email_verification()
            .check_token(token)
            .await
            .map_err(map_err)
    }

    /// Verify (consume) the token. Marks the user's `email_verified_at`
    /// timestamp and returns the freshly-updated [`User`].
    ///
    /// Fires [`crate::auth_flows::events::EmailVerified`] on success. The
    /// event dispatch is best-effort: a listener panic or transient
    /// dispatcher error does **not** roll back the verification. (See
    /// the module-level docs for rationale.)
    pub async fn verify(token: &str) -> Result<User, FrameworkError> {
        let user = instance()?
            .email_verification()
            .verify(token)
            .await
            .map_err(map_err)?;

        // Intentionally discard the dispatch error — verification has
        // already committed; a downstream listener failure must not
        // surface as a verification failure to the caller. The
        // dispatcher itself logs listener errors via tracing.
        let _ = crate::events::EventFacade::dispatch(
            crate::auth_flows::events::EmailVerified {
                user_id: user.id.to_string(),
            },
        )
        .await;

        Ok(user)
    }

    /// Generate a verification token, build the verification URL, and
    /// dispatch [`EmailVerificationMail`] via `Mail::to(&user.email).send(...)`.
    ///
    /// The URL has the shape `{base_url}?token={plaintext_token}`. A
    /// trailing slash on `base_url` is trimmed before the query string
    /// is appended.
    ///
    /// Reads `APP_NAME` and `MAIL_FROM` from the process environment for
    /// the outgoing subject and from-address. Both default — `"Suprnova"`
    /// and `"noreply@example.com"` — when unset.
    pub async fn send_link(user: &User, base_url: &str) -> Result<(), FrameworkError> {
        let token = Self::generate_token(&user.id).await?;
        let token_str = token
            .token()
            .ok_or_else(|| {
                FrameworkError::internal(
                    "torii email_verification.generate_token returned a token without plaintext",
                )
            })?
            .to_string();
        let url = format!(
            "{}?token={}",
            base_url.trim_end_matches('/'),
            token_str
        );

        let to_address = user.email.clone();
        let mail = EmailVerificationMail {
            to_address: to_address.clone(),
            user_name: user.name.clone(),
            verification_link: url,
            app_name: std::env::var("APP_NAME").unwrap_or_else(|_| "Suprnova".into()),
            from_address: std::env::var("MAIL_FROM")
                .unwrap_or_else(|_| "noreply@example.com".into()),
        };

        Mail::to(to_address.as_str()).send(mail).await
    }
}

fn map_err(e: torii::ToriiError) -> FrameworkError {
    FrameworkError::internal(format!("torii email verification: {e}"))
}
