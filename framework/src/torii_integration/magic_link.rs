//! Magic-link authentication facade.
//!
//! Wraps torii's `MagicLinkAuth` service behind the Suprnova `Auth::magic_link()` facade.
//!
//! # Mailer behaviour
//!
//! The underlying [`torii::MagicLinkAuth::send_link`] sends a magic-link email when a
//! mailer is configured on the Torii instance **and** the `mailer` feature is active.
//! In the current Phase-3 setup, neither is true — so `send_link` degrades to a pure
//! token-generation call (the `callback_url` is accepted but not emailed). Real email
//! delivery is Phase 5 (Queue + Mail) territory.
//!
//! The Suprnova facade **always returns the token string** so callers can emit it
//! themselves (e.g. embed it in a URL and write it to a log/channel for tests, or hand
//! it to a queue job for actual delivery later).
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::Auth;
//!
//! // Generate a magic-link token (and optionally email it, if a mailer is configured).
//! let token = Auth::magic_link()
//!     .send("alice@example.com", "http://localhost:8000/auth/magic")
//!     .await?;
//!
//! // The caller appends ?token=<token> to the callback URL and emails the user.
//!
//! // Once the user clicks the link, exchange the token for a (User, Session):
//! let (user, session) = Auth::magic_link().consume(&token).await?;
//! ```

use super::{Session, User, instance};
use crate::error::FrameworkError;

/// Facade for magic-link authentication operations.
///
/// Obtained via [`crate::Auth::magic_link()`].
pub struct MagicLinkAuth;

impl MagicLinkAuth {
    /// Generate a magic-link token for the given email address.
    ///
    /// If a mailer is configured on the Torii instance the user also receives an
    /// email containing `{callback_url}?token={token}`.  Without a mailer, this
    /// call simply creates and returns the token — the caller is responsible for
    /// delivering it.
    ///
    /// The email account is created on first use (torii's `get_or_create_user`
    /// semantics), so callers do not need to pre-register a user.
    ///
    /// # Errors
    ///
    /// Returns a [`FrameworkError`] if Torii is not initialised or if token
    /// creation fails.
    pub async fn send(&self, email: &str, callback_url: &str) -> Result<String, FrameworkError> {
        let torii = instance()?;
        let secure_token = torii
            .magic_link()
            .send_link(email, callback_url)
            .await
            .map_err(|e| FrameworkError::internal(format!("torii magic_link send_link: {e}")))?;

        // In the published torii-core 0.5.2, `SecureToken.token` is a plain `String`
        // public field — not a method or Option. No unwrapping required.
        Ok(secure_token.token)
    }

    /// Consume a magic-link token, returning the authenticated user and a new session.
    ///
    /// Tokens are **single-use** — calling `consume` a second time with the same
    /// token returns an error.
    ///
    /// # Errors
    ///
    /// Returns a [`FrameworkError`] if:
    /// - The token is invalid or has already been used.
    /// - Torii is not initialised.
    /// - Session creation fails.
    pub async fn consume(&self, token: &str) -> Result<(User, Session), FrameworkError> {
        let torii = instance()?;
        torii
            .magic_link()
            .authenticate(token, None, None)
            .await
            .map_err(|e| FrameworkError::internal(format!("torii magic_link authenticate: {e}")))
    }
}
