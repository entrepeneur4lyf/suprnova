//! `PasswordReset` — provider-backed password-reset facade.
//!
//! Mints, checks, and consumes reset tokens through the provider-agnostic
//! [`TokenStore`](crate::auth_flows::token_store::TokenStore) (the
//! `auth_flow_tokens` table), rotates the password through the application's
//! configured [`UserProvider`](crate::auth::UserProvider), and dispatches the
//! reset / changed emails through Suprnova's [`crate::Mail`] facade.
//!
//! [`PasswordReset::send_link`] dispatches [`crate::auth_flows::PasswordResetMail`]
//! and fires [`crate::auth_flows::events::PasswordResetLinkSent`].
//! [`PasswordReset::complete`] rotates the password, revokes every session and
//! remember-me token for the user, dispatches
//! [`crate::auth_flows::PasswordChangedMail`] as a fire-and-forget security
//! notification, and fires
//! [`crate::auth_flows::events::PasswordResetCompleted`].
//!
//! # No global auth instance
//!
//! Tokens live in the framework's own `auth_flow_tokens` table, not in any
//! particular auth backend, and the user lookup goes through whichever
//! [`UserProvider`](crate::auth::UserProvider) the app registered (the same one
//! [`Auth::user`](crate::auth::Auth::user) resolves against). There is no
//! global-instance initialization step and no provider-specific coupling — a
//! `send_link` / `complete` work purely by email and token.
//!
//! # Anti-enumeration semantics
//!
//! [`PasswordReset::send_link`] is anti-enumeration: callers cannot distinguish
//! "email exists" from "email does not exist" through the return type or
//! through whether mail was dispatched. When the email is absent **no token is
//! minted and no mail is sent**, and the absence is **not** leaked through an
//! `Err` — the method still returns `Ok(())`. The
//! [`crate::auth_flows::events::PasswordResetLinkSent`] event is likewise not
//! fired for an absent email, so a listener that counts events cannot
//! distinguish absent addresses.
//!
//! # Failure semantics on `complete()`
//!
//! The token is consumed (the single-use stamp) and the provider's
//! `set_password` both happen before sessions are revoked, before the
//! security-notification email is dispatched, and before the
//! [`crate::auth_flows::events::PasswordResetCompleted`] event fires. A
//! revocation failure, a mail-transport failure, or a listener panic therefore
//! cannot un-reset the password. We log those failures via tracing and discard
//! the event-dispatch error — a side-effect on a notification path must never
//! roll back a successful reset.

use crate::auth::active_user_provider;
use crate::auth_flows::mail::{PasswordChangedMail, PasswordResetMail};
use crate::auth_flows::token_store::{TokenPurpose, TokenStore};
use crate::error::FrameworkError;
use crate::mail::Mail;

/// Facade for password-reset token operations.
///
/// All methods operate over the framework's `auth_flow_tokens` table and the
/// application's configured [`UserProvider`](crate::auth::UserProvider) — no
/// global auth instance to initialise first. Mail goes out through the
/// [`crate::Mail`] facade.
///
/// # Example
///
/// ```rust,no_run
/// use suprnova::auth_flows::PasswordReset;
///
/// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
/// # let token_from_query = String::new();
/// # let new_password = String::new();
/// // From the "forgot password" form (anti-enumeration: an unknown address
/// // silently sends nothing):
/// PasswordReset::send_link("alice@example.com", "https://example.com/reset").await?;
///
/// // From the click-through handler, after the user enters a new password:
/// let user_id = PasswordReset::complete(&token_from_query, &new_password).await?;
/// # Ok(()) }
/// ```
pub struct PasswordReset;

impl PasswordReset {
    /// Send a password-reset link by email — the anti-enumeration entry point.
    ///
    /// Looks the user up through the active
    /// [`UserProvider`](crate::auth::UserProvider) and only mints + sends a
    /// token when an account is on file. An unknown email is a silent no-op: no
    /// token is issued, no mail is dispatched, no
    /// [`crate::auth_flows::events::PasswordResetLinkSent`] event fires, and the
    /// method still returns `Ok(())` so a caller (and a network observer) cannot
    /// distinguish "no such account" from "link sent."
    ///
    /// The reset URL has the shape `{base_url}?token={plaintext_token}` (a
    /// trailing slash on `base_url` is trimmed first; an existing query string
    /// gets `&` instead of `?`). The token is issued with
    /// [`TokenPurpose::PasswordReset`]'s default TTL (15 minutes).
    ///
    /// On the on-file path, fires
    /// [`crate::auth_flows::events::PasswordResetLinkSent`]. The dispatch is
    /// best-effort: a listener panic or transient dispatcher error is discarded
    /// (the token is already minted) and does not surface as an `Err`.
    ///
    /// Reads `APP_NAME` (defaults to `"Suprnova"`) and `MAIL_FROM` (required —
    /// errors if unset) from the process environment. Defaulting `MAIL_FROM` to
    /// a placeholder breaks DMARC/SPF in production, so the facade fails closed
    /// instead of silently sending from a domain the operator doesn't control.
    pub async fn send_link(email: &str, base_url: &str) -> Result<(), FrameworkError> {
        let Some(user) = active_user_provider()?.retrieve_by_email(email).await? else {
            // Anti-enumeration: absent account → no token, no mail, no signal.
            return Ok(());
        };

        // Validate the fail-closed `MAIL_FROM` read before issuing a token, so a
        // misconfigured sender fails fast without leaving an orphan token row.
        let from_address = crate::auth_flows::require_mail_from()?;

        let token = TokenStore::issue(
            &user.id,
            TokenPurpose::PasswordReset,
            TokenPurpose::PasswordReset.default_ttl(),
        )
        .await?;
        let url = crate::auth_flows::append_token_query(base_url, &token);

        let to_address = user.email.clone();
        let mail = PasswordResetMail {
            to_address: to_address.clone(),
            user_name: user.name,
            reset_link: url,
            app_name: crate::auth_flows::app_name(),
            from_address,
        };

        // The reset link is the primary action — propagate a transport failure
        // (the user requested it and is waiting on it). The audit event is the
        // best-effort side-channel: discard its dispatch error so a listener
        // panic can't fail the request after the mail already went out.
        Mail::to(to_address.as_str()).send(mail).await?;

        let _ = crate::events::EventFacade::dispatch(
            crate::auth_flows::events::PasswordResetLinkSent {
                user_id: user.id,
                email: to_address,
            },
        )
        .await;

        Ok(())
    }

    /// Check whether `token` is a live, unused reset token without consuming it.
    ///
    /// Useful for landing pages that want to confirm the token before rendering
    /// the new-password form, so a refresh does not burn the token.
    pub async fn check(token: &str) -> Result<bool, FrameworkError> {
        TokenStore::check(token, TokenPurpose::PasswordReset).await
    }

    /// Consume `token` (single-use) and rotate the user's password to
    /// `new_password`, returning the user's id.
    ///
    /// Side effects, in order:
    ///
    /// 1. The token is consumed (single-use; a second `complete` on the same
    ///    token returns an error) and the new password is hashed with
    ///    [`crate::hashing::hash`] and stored through the active
    ///    [`UserProvider`](crate::auth::UserProvider). The provider stores the
    ///    value verbatim, so the facade hashes before handing it over.
    /// 2. Every session row and every remember-me row for the user is revoked.
    ///    A stolen session must not outlive the credential it depended on. Both
    ///    are best-effort: failures log via `tracing` but do **not** roll back
    ///    the committed password change.
    /// 3. A [`PasswordChangedMail`] security notification is dispatched through
    ///    the [`Mail`] facade, addressed via the provider's
    ///    [`flow_user_by_id`](crate::auth::UserProvider::flow_user_by_id). If
    ///    the user vanished or the send fails, the failure is logged and the
    ///    method proceeds — the password is already rotated.
    /// 4. A [`crate::auth_flows::events::PasswordResetCompleted`] event is fired.
    ///    A dispatcher error is discarded (the dispatcher logs listener errors
    ///    via its own tracing instrumentation).
    ///
    /// Reads `APP_NAME` (defaults to `"Suprnova"`) and `MAIL_FROM` (required for
    /// the notification — a missing `MAIL_FROM` only skips the best-effort
    /// notification; the password change itself still commits).
    ///
    /// # Errors
    ///
    /// - [`crate::FrameworkError::bad_request`] (400) when `new_password` is
    ///   empty/whitespace, or when the token is invalid, already consumed, or
    ///   expired.
    /// - Whatever the provider returns from `set_password` when the storage
    ///   layer fails.
    /// - The "no provider configured" error from the active-user-provider
    ///   resolver when no `UserProvider` is registered.
    pub async fn complete(token: &str, new_password: &str) -> Result<String, FrameworkError> {
        if new_password.trim().is_empty() {
            return Err(FrameworkError::bad_request(
                "new_password must not be empty",
            ));
        }

        // Consume the token first (single-use). If the rotation that follows
        // fails, the token is already spent and a fresh reset link is required —
        // the ordering is deliberate: reversing it would leave a reusable token
        // behind a failed rotation.
        let id = TokenStore::consume(token, TokenPurpose::PasswordReset)
            .await?
            .ok_or_else(|| FrameworkError::bad_request("invalid or expired reset token"))?;

        // The provider stores the password verbatim — hash here.
        let hashed = crate::hashing::hash(new_password)?;
        active_user_provider()?.set_password(&id, &hashed).await?;

        // Revoke every session + remember-me row for this user. A stolen
        // session must not outlive the credential it depended on; same for any
        // persistent remember-me cookie. Both are best-effort: failures log but
        // do not roll back the committed password change.
        match crate::session::destroy_all_for_user(&id).await {
            Ok(n) => {
                if n > 0 {
                    tracing::info!("revoked {n} session row(s) for user {id} after password reset");
                }
            }
            Err(e) => {
                tracing::warn!("session revocation failed for user {id} after password reset: {e}");
            }
        }
        match crate::auth::remember::revoke_all_for_user(&id).await {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(
                        "revoked {n} remember-me row(s) for user {id} after password reset"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "remember-me revocation failed for user {id} after password reset: {e}"
                );
            }
        }

        // Fire-and-forget security notification. Source the recipient email via
        // the provider's flow_user_by_id (the user struct's email_for_reset). A
        // vanished user or a delivery failure here must not roll back the
        // already-committed password change — log and proceed.
        match active_user_provider()?.flow_user_by_id(&id).await {
            Ok(Some(u)) => match crate::auth_flows::require_mail_from() {
                Ok(from_address) => {
                    let to_address = u.email;
                    let mail = PasswordChangedMail {
                        to_address: to_address.clone(),
                        user_name: u.name,
                        app_name: crate::auth_flows::app_name(),
                        from_address,
                    };
                    if let Err(e) = Mail::to(to_address.as_str()).send(mail).await {
                        tracing::warn!(
                            "password-changed security notification failed for user {id}: {e}"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "password-changed security notification skipped for user {id}: {e}"
                    );
                }
            },
            Ok(None) => {
                tracing::warn!(
                    "password-changed security notification skipped: user {id} not found after reset"
                );
            }
            Err(e) => {
                tracing::warn!(
                    "password-changed security notification skipped for user {id}: lookup failed: {e}"
                );
            }
        }

        // Intentionally discard the dispatch error — the reset has already
        // committed; a downstream listener failure must not surface as a reset
        // failure to the caller. The dispatcher itself logs listener errors via
        // tracing.
        let _ = crate::events::EventFacade::dispatch(
            crate::auth_flows::events::PasswordResetCompleted {
                user_id: id.clone(),
            },
        )
        .await;

        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The provider-backed paths (`send_link` / `complete`) need a real
    // `UserProvider` + DB and are covered by the integration test in
    // `framework/tests/password_reset.rs`. The one branch that needs no setup is
    // the empty-password guard in `complete`: it returns `bad_request` before
    // touching the token store or the provider, so it can be exercised here.
    #[tokio::test]
    async fn complete_rejects_empty_password_before_touching_the_store() {
        assert!(
            PasswordReset::complete("any-token", "   ").await.is_err(),
            "an empty/whitespace password must be rejected up front"
        );
        assert!(
            PasswordReset::complete("any-token", "").await.is_err(),
            "an empty password must be rejected up front"
        );
    }
}
