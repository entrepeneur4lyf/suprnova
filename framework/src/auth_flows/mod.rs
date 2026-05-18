//! Phase 11 — Auth Flows.
//!
//! Built on torii for email-verification, password-reset, and
//! brute-force-protection lifecycle. 2FA and remember-me are ours.
//! Mail delivery for all torii-flow emails routes through Suprnova's
//! [`crate::Mail`] facade — torii's own `mailer` feature is intentionally
//! disabled.
//!
//! See `docs/core/auth-flows.md` for usage.

pub mod mail;

pub use mail::{EmailVerificationMail, PasswordChangedMail, PasswordResetMail};
