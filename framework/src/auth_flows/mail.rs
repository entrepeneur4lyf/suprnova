//! Transactional [`Mailable`] types for the auth flows.
//!
//! These three mailables back the email-verification, password-reset, and
//! password-changed lifecycle the [`EmailVerification`](crate::auth_flows::EmailVerification)
//! / [`PasswordReset`](crate::auth_flows::PasswordReset) facades drive. They
//! are dispatched via the ordinary [`Mail`](crate::Mail) facade:
//!
//! ```rust,no_run
//! # use suprnova::Mail;
//! # use suprnova::auth_flows::EmailVerificationMail;
//! # async fn ex(mail: EmailVerificationMail) -> Result<(), Box<dyn std::error::Error>> {
//! Mail::to(mail.to_address.as_str()).send(mail).await?;
//! # Ok(()) }
//! ```
//!
//! `to_address` and `from_address` live on each struct as plain `String`s so
//! they (a) participate in the serialized Tera context the template renders
//! against and (b) survive the JSON round-trip the queue worker performs.
//! The `from()` impl converts `from_address` into an [`Address`] for the
//! mail dispatcher.
//!
//! # Escaping
//!
//! The mail crate disables Tera autoescape (see
//! `framework/src/mail/mailable.rs`). User-controllable fields rendered into
//! the HTML body are piped through Tera's built-in `escape` filter so an
//! attacker cannot smuggle markup through a chosen display name. The text
//! body does not escape because its consumers (mail clients in
//! plaintext-mode) render it verbatim and `&` / `<` are not special there.

use crate::mail::{Address, Mailable};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────
// EmailVerificationMail
// ──────────────────────────────────────────────────────────────────────

/// "Verify your email" message dispatched after signup (and on resend).
///
/// The `verification_link` is the fully-qualified URL the
/// [`EmailVerification`](crate::auth_flows::EmailVerification) facade builds
/// (base URL + the issued token as a query parameter) — the mailable does not
/// construct or sign the token itself.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EmailVerificationMail {
    /// Recipient. Used both as the Tera context and on the call site
    /// (`Mail::to(&mail.to_address).send(mail)`).
    pub to_address: String,
    /// Optional display name. When `None` the templates fall back to
    /// "there" via Tera's `default` filter.
    pub user_name: Option<String>,
    /// Fully-qualified verification URL.
    pub verification_link: String,
    /// Display name of the sending application (interpolated into the
    /// subject and the body's branding line).
    pub app_name: String,
    /// Envelope `From`. Plain `String` for serde-friendliness; the
    /// `from()` impl lifts it into an [`Address`].
    pub from_address: String,
}

#[async_trait]
impl Mailable for EmailVerificationMail {
    fn mailable_name() -> &'static str {
        "EmailVerificationMail"
    }

    fn subject(&self) -> String {
        format!("Verify your email for {}", self.app_name)
    }

    fn html_template_source(&self) -> Option<String> {
        // Autoescape is OFF — pipe user-controllable fields through `escape`
        // explicitly. `app_name` and `verification_link` originate from
        // framework-controlled config, but we still escape them so a
        // future config typo (`<` in the brand string) can't break rendering.
        Some(
            r#"<!doctype html>
<html>
  <body style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; color: #1a1a1a;">
    <h1 style="font-size: 20px;">Hi {{ user_name | default(value="there") | escape }},</h1>
    <p>Welcome to {{ app_name | escape }}. Please confirm your email address by clicking the link below:</p>
    <p><a href="{{ verification_link | escape }}" style="display: inline-block; padding: 10px 16px; background: #2563eb; color: #fff; text-decoration: none; border-radius: 6px;">Verify email</a></p>
    <p>Or copy this URL into your browser:<br><span style="word-break: break-all;">{{ verification_link | escape }}</span></p>
    <p>This link expires in 24 hours. If you didn't sign up for {{ app_name | escape }}, you can safely ignore this email.</p>
  </body>
</html>"#
                .to_string(),
        )
    }

    fn text_template_source(&self) -> Option<String> {
        Some(
            "Hi {{ user_name | default(value=\"there\") }},\n\
             \n\
             Welcome to {{ app_name }}. Please confirm your email address by visiting:\n\
             \n\
             {{ verification_link }}\n\
             \n\
             This link expires in 24 hours. If you didn't sign up for {{ app_name }}, \
             you can safely ignore this email.\n"
                .to_string(),
        )
    }

    fn from(&self) -> Option<Address> {
        Some(Address::new(&self.from_address))
    }
}

// ──────────────────────────────────────────────────────────────────────
// PasswordResetMail
// ──────────────────────────────────────────────────────────────────────

/// "Reset your password" message dispatched when the user requests a
/// password-reset link from the forgot-password endpoint.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PasswordResetMail {
    /// Recipient address — must be the user's on-file email.
    pub to_address: String,
    /// Display name interpolated into the greeting; `None` falls back to the email local-part.
    pub user_name: Option<String>,
    /// Fully-qualified reset URL.
    pub reset_link: String,
    /// Application name used in the subject + body.
    pub app_name: String,
    /// Envelope-from address for the outgoing message.
    pub from_address: String,
}

#[async_trait]
impl Mailable for PasswordResetMail {
    fn mailable_name() -> &'static str {
        "PasswordResetMail"
    }

    fn subject(&self) -> String {
        format!("Reset your {} password", self.app_name)
    }

    fn html_template_source(&self) -> Option<String> {
        Some(
            r#"<!doctype html>
<html>
  <body style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; color: #1a1a1a;">
    <h1 style="font-size: 20px;">Hi {{ user_name | default(value="there") | escape }},</h1>
    <p>We received a request to reset your {{ app_name | escape }} password. Click the link below to choose a new one:</p>
    <p><a href="{{ reset_link | escape }}" style="display: inline-block; padding: 10px 16px; background: #2563eb; color: #fff; text-decoration: none; border-radius: 6px;">Reset password</a></p>
    <p>Or copy this URL into your browser:<br><span style="word-break: break-all;">{{ reset_link | escape }}</span></p>
    <p>This link expires in 15 minutes. If you didn't request a password reset, you can safely ignore this email — your password will stay the same.</p>
  </body>
</html>"#
                .to_string(),
        )
    }

    fn text_template_source(&self) -> Option<String> {
        Some(
            "Hi {{ user_name | default(value=\"there\") }},\n\
             \n\
             We received a request to reset your {{ app_name }} password. \
             Visit the link below to choose a new one:\n\
             \n\
             {{ reset_link }}\n\
             \n\
             This link expires in 15 minutes. If you didn't request a password reset, \
             you can safely ignore this email — your password will stay the same.\n"
                .to_string(),
        )
    }

    fn from(&self) -> Option<Address> {
        Some(Address::new(&self.from_address))
    }
}

// ──────────────────────────────────────────────────────────────────────
// PasswordChangedMail
// ──────────────────────────────────────────────────────────────────────

/// "Your password was changed" confirmation dispatched after a successful
/// password change (via the reset flow, the change-password endpoint, or any
/// other lifecycle event that mutates the password hash).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PasswordChangedMail {
    /// Recipient address — the user's on-file email.
    pub to_address: String,
    /// Display name interpolated into the greeting; `None` falls back to the email local-part.
    pub user_name: Option<String>,
    /// Application name used in the subject + body.
    pub app_name: String,
    /// Envelope-from address for the outgoing message.
    pub from_address: String,
}

#[async_trait]
impl Mailable for PasswordChangedMail {
    fn mailable_name() -> &'static str {
        "PasswordChangedMail"
    }

    fn subject(&self) -> String {
        format!("Your {} password was changed", self.app_name)
    }

    fn html_template_source(&self) -> Option<String> {
        Some(
            r#"<!doctype html>
<html>
  <body style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; color: #1a1a1a;">
    <h1 style="font-size: 20px;">Hi {{ user_name | default(value="there") | escape }},</h1>
    <p>Your {{ app_name | escape }} password was just changed.</p>
    <p>If this was you, no further action is required.</p>
    <p>If this <strong>wasn't</strong> you, please contact our support team immediately so we can secure your account.</p>
  </body>
</html>"#
                .to_string(),
        )
    }

    fn text_template_source(&self) -> Option<String> {
        Some(
            "Hi {{ user_name | default(value=\"there\") }},\n\
             \n\
             Your {{ app_name }} password was just changed.\n\
             \n\
             If this was you, no further action is required.\n\
             \n\
             If this WASN'T you, please contact our support team immediately so we can \
             secure your account.\n"
                .to_string(),
        )
    }

    fn from(&self) -> Option<Address> {
        Some(Address::new(&self.from_address))
    }
}

// ──────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::Mailable;

    #[test]
    fn email_verification_html_escapes_user_name() {
        let m = EmailVerificationMail {
            to_address: "x@example.com".into(),
            user_name: Some("<script>alert(1)</script>".into()),
            verification_link: "https://example.com/v?t=x".into(),
            app_name: "App".into(),
            from_address: "no@reply.com".into(),
        };
        let html = m.render_html().unwrap().unwrap();
        assert!(!html.contains("<script>"), "raw script tag escaped: {html}");
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn password_reset_text_preserves_link_verbatim() {
        let m = PasswordResetMail {
            to_address: "x@example.com".into(),
            user_name: None,
            reset_link: "https://example.com/r?t=abc&u=42".into(),
            app_name: "App".into(),
            from_address: "no@reply.com".into(),
        };
        let text = m.render_text().unwrap().unwrap();
        // Plain text doesn't HTML-escape, so the & is preserved.
        assert!(text.contains("https://example.com/r?t=abc&u=42"));
    }

    #[test]
    fn missing_user_name_falls_back_to_there() {
        let m = PasswordChangedMail {
            to_address: "x@example.com".into(),
            user_name: None,
            app_name: "App".into(),
            from_address: "no@reply.com".into(),
        };
        let html = m.render_html().unwrap().unwrap();
        assert!(html.contains("Hi there"), "missing name fallback: {html}");
    }

    #[test]
    fn subject_includes_app_name() {
        let m = EmailVerificationMail {
            to_address: "x@example.com".into(),
            user_name: None,
            verification_link: "https://x".into(),
            app_name: "MyCorp".into(),
            from_address: "no@reply.com".into(),
        };
        assert_eq!(m.render_subject().unwrap(), "Verify your email for MyCorp");
    }
}
