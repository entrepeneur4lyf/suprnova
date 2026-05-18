//! Phase 11 — Auth Flows.
//!
//! Built on torii for email-verification, password-reset, and
//! brute-force-protection lifecycle. 2FA and remember-me are ours.
//! Mail delivery for all torii-flow emails routes through Suprnova's
//! [`crate::Mail`] facade — torii's own `mailer` feature is intentionally
//! disabled.
//!
//! See `docs/core/auth-flows.md` for usage.

pub mod brute_force;
pub mod email_verify;
pub mod events;
pub mod mail;
pub mod password_reset;
pub mod two_factor;

pub use brute_force::{BruteForce, LoginThrottleMiddleware};
pub use email_verify::EmailVerification;
pub use events::{
    AccountUnlocked, EmailVerified, PasswordResetCompleted, TwoFactorDisabled, TwoFactorEnrolled,
};
pub use mail::{EmailVerificationMail, PasswordChangedMail, PasswordResetMail};
pub use password_reset::PasswordReset;
pub use two_factor::{EnrollmentResponse, TwoFactor, TwoFactorUser};
