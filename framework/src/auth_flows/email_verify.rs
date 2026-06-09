//! `EmailVerification` — provider-backed email-verification facade.
//!
//! Mints, checks, and consumes verification tokens through the
//! provider-agnostic [`TokenStore`](crate::auth_flows::token_store::TokenStore)
//! (the `auth_flow_tokens` table), marks the user verified through the
//! application's configured [`UserProvider`](crate::auth::UserProvider), and
//! dispatches the verification email through Suprnova's [`crate::Mail`] facade.
//! Verification fires an [`EmailVerified`](crate::auth_flows::events::EmailVerified)
//! event so listeners can react (e.g. unlock additional functionality, send a
//! welcome email).
//!
//! # No global auth instance
//!
//! Tokens live in the framework's own `auth_flow_tokens` table, not in any
//! particular auth backend, and the user lookup goes through whichever
//! [`UserProvider`](crate::auth::UserProvider) the app registered (the same
//! one [`Auth::user`](crate::auth::Auth::user) resolves against). There is no
//! global-instance initialization step and no provider-specific coupling — a
//! `send_link` takes any [`MustVerifyEmail`] user, and `resend` / `verify`
//! work purely by email and token.
//!
//! # Failure semantics on `verify()`
//!
//! Token consumption (the single-use stamp) and the provider's
//! `mark_email_verified` both happen before the `EmailVerified` event fires. A
//! listener panic or transient event-dispatcher error therefore cannot
//! un-verify the user. We discard the dispatch error (the dispatcher logs
//! listener failures via its own tracing instrumentation) but return the
//! user id regardless — a side-effect on a notification path must never roll
//! back a successful verification.

use crate::auth::active_user_provider;
use crate::auth::must_verify_email::MustVerifyEmail;
use crate::auth_flows::mail::EmailVerificationMail;
use crate::auth_flows::token_store::{TokenPurpose, TokenStore};
use crate::error::FrameworkError;
use crate::mail::Mail;

/// Facade for email-verification token operations.
///
/// All methods operate over the framework's `auth_flow_tokens` table and the
/// application's configured [`UserProvider`](crate::auth::UserProvider) — no
/// global auth instance to initialise first. Mail goes out through the
/// [`crate::Mail`] facade.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::auth_flows::EmailVerification;
///
/// // After a fresh signup, with the freshly-created user in hand:
/// EmailVerification::send_link(&user, "https://example.com/verify").await?;
///
/// // A "resend the link" endpoint takes only the email (anti-enumeration:
/// // an unknown address silently sends nothing):
/// EmailVerification::resend("alice@example.com", "https://example.com/verify").await?;
///
/// // From the click-through handler:
/// let user_id = EmailVerification::verify(&token_from_query).await?;
/// ```
pub struct EmailVerification;

impl EmailVerification {
    /// Mint a verification token for `user`, build the verification URL, and
    /// dispatch [`EmailVerificationMail`] to the user's email via the
    /// [`crate::Mail`] facade.
    ///
    /// The URL has the shape `{base_url}?token={plaintext_token}` (a trailing
    /// slash on `base_url` is trimmed first; an existing query string gets `&`
    /// instead of `?`). The token is issued with
    /// [`TokenPurpose::EmailVerification`]'s default TTL (24h).
    ///
    /// Reads `APP_NAME` (defaults to `"Suprnova"`) and `MAIL_FROM`
    /// (required — errors if unset) from the process environment. Defaulting
    /// `MAIL_FROM` to a placeholder breaks DMARC/SPF in production, so the
    /// facade fails closed instead of silently sending from a domain the
    /// operator doesn't control.
    pub async fn send_link<U: MustVerifyEmail>(
        user: &U,
        base_url: &str,
    ) -> Result<(), FrameworkError> {
        Self::issue_and_mail(
            &user.get_auth_identifier(),
            user.email(),
            user.name().map(str::to_string),
            base_url,
        )
        .await
    }

    /// Issue an email-verification token for `id`, append it to `base_url`, and
    /// send the verification mail to `email` (greeting `name` if present).
    ///
    /// Shared pipeline for [`send_link`](Self::send_link) and
    /// [`resend`](Self::resend): mint with [`TokenPurpose::EmailVerification`]'s
    /// default TTL, build the `{base_url}?token=…` URL via
    /// [`append_token_query`](crate::auth_flows::append_token_query), read
    /// `APP_NAME` / `MAIL_FROM` (fail-closed), and dispatch through the
    /// [`crate::Mail`] facade.
    async fn issue_and_mail(
        id: &str,
        email: &str,
        name: Option<String>,
        base_url: &str,
    ) -> Result<(), FrameworkError> {
        let token = TokenStore::issue(
            id,
            TokenPurpose::EmailVerification,
            TokenPurpose::EmailVerification.default_ttl(),
        )
        .await?;
        let url = crate::auth_flows::append_token_query(base_url, &token);

        let mail = EmailVerificationMail {
            to_address: email.to_string(),
            user_name: name,
            verification_link: url,
            app_name: crate::auth_flows::app_name(),
            from_address: crate::auth_flows::require_mail_from()?,
        };

        Mail::to(email).send(mail).await
    }

    /// Resend a verification link by email — the anti-enumeration entry point.
    ///
    /// Looks the user up through the active
    /// [`UserProvider`](crate::auth::UserProvider) and only mints + sends a
    /// token when an account is on file. An unknown email is a silent no-op:
    /// no token is issued, no mail is dispatched, and the method still returns
    /// `Ok(())` so a caller (and a network observer) cannot distinguish
    /// "no such account" from "link sent."
    ///
    /// `base_url` is the verification landing URL; the same `{base_url}?token=…`
    /// shape and `MAIL_FROM` / `APP_NAME` rules as [`send_link`](Self::send_link)
    /// apply.
    pub async fn resend(email: &str, base_url: &str) -> Result<(), FrameworkError> {
        let Some(user) = active_user_provider()?.retrieve_by_email(email).await? else {
            // Anti-enumeration: absent account → no token, no mail, no signal.
            return Ok(());
        };

        Self::issue_and_mail(&user.id, &user.email, user.name, base_url).await?;
        Ok(())
    }

    /// Check whether `token` is a live, unused verification token without
    /// consuming it.
    ///
    /// Useful for landing pages that want to display a "click to verify"
    /// button before actually consuming the token (so a refresh does not burn
    /// the token).
    pub async fn check(token: &str) -> Result<bool, FrameworkError> {
        TokenStore::check(token, TokenPurpose::EmailVerification).await
    }

    /// Consume the verification token, mark the user verified through the
    /// active [`UserProvider`](crate::auth::UserProvider), and return the
    /// user's id.
    ///
    /// Single-use: a second `verify` on the same token returns an error (the
    /// [`TokenStore`] stamps `used_at` atomically). An invalid or expired
    /// token also errors.
    ///
    /// Fires [`crate::auth_flows::events::EmailVerified`] on success. The
    /// event dispatch is best-effort: a listener panic or transient dispatcher
    /// error does **not** roll back the verification. (See the module-level
    /// docs for rationale.)
    ///
    /// Note: the token is consumed (single-use) before the provider marks the
    /// user verified; if the mark step errors, the token is already spent and a
    /// fresh verification link is required. The ordering is deliberate —
    /// reversing it would leave a reusable token behind a failed mark.
    ///
    /// # Errors
    ///
    /// - [`crate::FrameworkError::bad_request`] (400) when the token is
    ///   invalid, already consumed, or expired.
    /// - Whatever the provider returns from `mark_email_verified` when the
    ///   storage layer fails.
    /// - The "no provider configured" error from
    ///   [`active_user_provider`](crate::auth::active_user_provider) when no
    ///   `UserProvider` is registered.
    pub async fn verify(token: &str) -> Result<String, FrameworkError> {
        let user_id = TokenStore::consume(token, TokenPurpose::EmailVerification)
            .await?
            .ok_or_else(|| {
                FrameworkError::bad_request("invalid or expired verification token")
            })?;

        active_user_provider()?.mark_email_verified(&user_id).await?;

        // Intentionally discard the dispatch error — verification has already
        // committed; a downstream listener failure must not surface as a
        // verification failure to the caller. The dispatcher itself logs
        // listener errors via tracing.
        let _ = crate::events::EventFacade::dispatch(crate::auth_flows::events::EmailVerified {
            user_id: user_id.clone(),
        })
        .await;

        Ok(user_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `send_link` is generic over `MustVerifyEmail`. A minimal in-crate user
    // exercises the generic bound + the email/name accessors without pulling
    // in the model macro (which only self-resolves `::suprnova` from an
    // external test crate). The provider-backed paths (`resend`/`verify`)
    // need a real `UserProvider` + DB and are covered by the integration test
    // in `framework/tests/email_verify.rs`.
    use crate::auth::Authenticatable;
    use chrono::{DateTime, Utc};
    use std::any::Any;
    use std::sync::Arc;

    struct Signup {
        id: String,
        email: String,
        name: Option<String>,
    }

    impl Authenticatable for Signup {
        fn get_auth_identifier(&self) -> String {
            self.id.clone()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
            self
        }
    }

    impl MustVerifyEmail for Signup {
        fn email(&self) -> &str {
            &self.email
        }
        fn email_verified_at(&self) -> Option<DateTime<Utc>> {
            None
        }
        fn set_email_verified_at(&mut self, _v: Option<DateTime<Utc>>) {}
        fn name(&self) -> Option<&str> {
            self.name.as_deref()
        }
    }

    // `send_link` mints a token (needs the `auth_flow_tokens` table) and sends
    // through `Mail::fake`. We assert the recipient + that the rendered link
    // carries the issued token through `append_token_query` — the full
    // facade-to-mail wiring on the generic user path.
    #[tokio::test]
    #[serial_test::serial]
    async fn send_link_mails_a_token_link_to_the_user() {
        use sea_orm::ConnectionTrait;
        let db = crate::testing::TestDatabase::sqlite_memory()
            .await
            .expect("sqlite_memory");
        let conn = db.conn();
        let stmt = crate::auth_flows::token_store::create_auth_flow_tokens_table();
        conn.execute(conn.get_database_backend().build(&stmt))
            .await
            .expect("create auth_flow_tokens table");

        // `send_link` reads MAIL_FROM (fail-closed). Set it for the test.
        // SAFETY: serialized by `#[serial]`; no parallel observer.
        unsafe {
            std::env::set_var("MAIL_FROM", "test-mailer@example.com");
        }

        let fake = Mail::fake();
        let user = Signup {
            id: "42".to_string(),
            email: "ada@example.com".to_string(),
            name: Some("Ada".to_string()),
        };

        EmailVerification::send_link(&user, "https://app.test/verify-email/verify")
            .await
            .expect("send_link");

        fake.assert_sent_to("ada@example.com");

        // The rendered text body carries the verification link verbatim
        // (`?token=<plaintext>`); the HTML body HTML-escapes the slashes, so
        // assert against the text body for the literal URL prefix.
        let captured = fake.captured();
        assert_eq!(captured.len(), 1, "exactly one mail sent");
        let text = captured[0]
            .text
            .as_deref()
            .expect("verification mail has a text body");
        assert!(
            text.contains("https://app.test/verify-email/verify?token="),
            "rendered link must carry the issued token; got: {text}"
        );
    }
}
