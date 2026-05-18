//! Phase 11 — Auth Flows.
//!
//! Five auth-flow features behind one cohesive module:
//!
//! - [`EmailVerification`] — torii-backed verification tokens, mail
//!   dispatched via Suprnova's [`crate::Mail`] facade.
//! - [`PasswordReset`] — torii-backed reset tokens with anti-enumeration
//!   `send_link`, fire-and-forget [`PasswordChangedMail`] notification.
//! - [`BruteForce`] + [`LoginThrottleMiddleware`] — torii-backed lockout
//!   plus an HTTP middleware that 429s pre-handler when the targeted
//!   account is locked.
//! - [`TwoFactor`] — TOTP enrollment + verification + recovery codes.
//!   Framework-owned storage (`two_factor_credentials` table), secrets
//!   and recovery codes encrypted at rest via [`crate::crypto::Crypt`].
//! - [`remember_me`] — re-export of [`crate::auth::remember`]. The
//!   stronger DB-row + bcrypt + single-use rotation design that
//!   shipped with the auth module; listed here for namespace cohesion.
//!
//! All flows dispatch transactional emails through the same
//! [`crate::Mail`] facade — torii's optional `mailer` feature is
//! intentionally disabled (see `framework/Cargo.toml`).
//!
//! See `docs/core/auth-flows.md` for end-to-end usage.

pub mod brute_force;
pub mod email_verify;
pub mod events;
pub mod mail;
pub mod password_reset;
pub mod remember_me;
pub mod two_factor;

pub use brute_force::{BruteForce, LoginThrottleMiddleware};
pub use email_verify::EmailVerification;
pub use events::{
    AccountLocked, AccountUnlocked, EmailVerified, PasswordResetCompleted, TwoFactorDisabled,
    TwoFactorEnrolled,
};
pub use mail::{EmailVerificationMail, PasswordChangedMail, PasswordResetMail};
pub use password_reset::PasswordReset;
pub use two_factor::{EnrollmentResponse, TwoFactor, TwoFactorUser};
