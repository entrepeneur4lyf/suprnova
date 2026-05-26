//! Magic-link authentication facade.
//!
//! Wraps torii's `MagicLinkAuth` service behind the Suprnova `Auth::magic_link()` facade.
//!
//! # Email delivery
//!
//! Suprnova owns the [`crate::Mail`] facade — torii's optional `mailer` feature
//! is intentionally disabled. The facade always returns the plaintext token to
//! the caller, who is responsible for shipping it to the user (typically by
//! constructing `{callback_url}?token={token}` and dispatching a `Mailable`
//! via `Mail::send`).
//!
//! For email-verification, password-reset, and password-changed emails the
//! framework provides ready-made flows in [`crate::auth_flows`] that wire
//! token generation to a [`crate::mail::Mailable`]. Magic-link delivery is
//! left to the application because the message body is product-specific.
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::Auth;
//!
//! // Generate a magic-link token. The framework does NOT email it for you.
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
    /// Returns the plaintext token — the caller assembles the URL
    /// (`{callback_url}?token={token}`) and emails it. Suprnova does not
    /// dispatch the magic-link email automatically; build a [`crate::mail::Mailable`]
    /// and call [`crate::Mail::send`].
    ///
    /// The user account is created on first use (torii's `get_or_create_user`
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
            .map_err(super::map_torii_error)?;

        // In the suprnova-torii-rs fork, `SecureToken::token` is a private field
        // wrapping a `SecretString`, exposed via `.token()` which returns the
        // plaintext as `Option<&str>` — `None` only when the token was loaded
        // from storage (hash only). Freshly minted tokens from `send_link`
        // always carry the plaintext, so the `None` branch indicates a torii
        // contract violation (or a torii bug) and is mapped to an internal
        // error.
        secure_token.token().map(str::to_owned).ok_or_else(|| {
            FrameworkError::internal(
                "torii magic_link send_link returned a token without plaintext",
            )
        })
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
            .map_err(super::map_torii_error)
    }
}
