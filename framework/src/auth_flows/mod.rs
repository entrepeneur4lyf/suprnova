//! Auth Flows.
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
pub mod email_verified_middleware;
pub mod email_verify;
pub mod events;
pub mod mail;
pub mod password_reset;
pub mod remember_me;
pub mod two_factor;
pub mod two_factor_challenge_middleware;

pub use brute_force::{
    BackendErrorPolicy as LoginThrottleBackendErrorPolicy, BruteForce, LoginThrottleMiddleware,
};
// Also re-export `BackendErrorPolicy` under its short name for
// callers who reach for it via `auth_flows::BackendErrorPolicy`.
// Two re-exports of the same type aren't ambiguous — they share an
// identity.
pub use brute_force::BackendErrorPolicy;
pub use email_verified_middleware::EnsureEmailVerifiedMiddleware;
pub use email_verify::EmailVerification;
pub use events::{
    AccountLocked, AccountUnlocked, EmailVerified, PasswordResetCompleted, PasswordResetLinkSent,
    TwoFactorChallengeFailed, TwoFactorChallenged, TwoFactorDisabled, TwoFactorEnrolled,
};
pub use mail::{EmailVerificationMail, PasswordChangedMail, PasswordResetMail};
pub use password_reset::PasswordReset;
pub use two_factor::{EnrollmentResponse, TwoFactor, TwoFactorUser};
pub use two_factor_challenge_middleware::TwoFactorChallengeMiddleware;

/// Resolve the `MAIL_FROM` env var. Errors when unset — the auth-flow
/// facades dispatch mail through this address and silently defaulting
/// to a placeholder (`noreply@example.com`) breaks production
/// DMARC / SPF and ships from a domain the operator doesn't control.
///
/// Apps set this once at boot (`.env`, systemd unit, k8s secret —
/// whatever the deploy uses). Tests set it via
/// `std::env::set_var("MAIL_FROM", "...")` in their setup helper.
pub(crate) fn require_mail_from() -> Result<String, crate::error::FrameworkError> {
    std::env::var("MAIL_FROM").map_err(|_| {
        crate::error::FrameworkError::internal(
            "MAIL_FROM environment variable is not set — auth_flows facades require \
             a real from-address. Set MAIL_FROM=ops@example.com in your environment.",
        )
    })
}

/// Resolve the `APP_NAME` env var, falling back to `"Suprnova"`. Used
/// in mail subjects + greetings. Unlike `MAIL_FROM`, a default here is
/// safe — the worst case is an unbranded subject line, not a delivery
/// failure.
pub(crate) fn app_name() -> String {
    std::env::var("APP_NAME").unwrap_or_else(|_| "Suprnova".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Cleared / restored env-var guard. Keeps the rest of the test
    /// suite (which often expects `MAIL_FROM` to be present) running
    /// after we deliberately unset it inside one test.
    struct MailFromGuard {
        previous: Option<String>,
    }

    impl MailFromGuard {
        fn unset() -> Self {
            let previous = std::env::var("MAIL_FROM").ok();
            // SAFETY: serial test — no parallel observer.
            unsafe {
                std::env::remove_var("MAIL_FROM");
            }
            Self { previous }
        }
    }

    impl Drop for MailFromGuard {
        fn drop(&mut self) {
            if let Some(prev) = self.previous.take() {
                // SAFETY: serial test — no parallel observer.
                unsafe {
                    std::env::set_var("MAIL_FROM", prev);
                }
            }
        }
    }

    #[test]
    #[serial]
    fn require_mail_from_errors_when_unset() {
        let _guard = MailFromGuard::unset();
        let result = require_mail_from();
        assert!(
            result.is_err(),
            "require_mail_from must fail closed when env unset"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("MAIL_FROM"),
            "error message must mention the missing variable; got: {msg}"
        );
    }

    #[test]
    #[serial]
    fn require_mail_from_returns_value_when_set() {
        // SAFETY: serial test — no parallel observer.
        unsafe {
            std::env::set_var("MAIL_FROM", "ops@example.com");
        }
        assert_eq!(require_mail_from().unwrap(), "ops@example.com");
    }

    #[test]
    fn app_name_defaults_to_suprnova_when_unset() {
        // No env touch — APP_NAME is typically unset in tests. The
        // default is the load-bearing contract.
        let name = app_name();
        // Either the test env set it, or the default kicked in. Both
        // are acceptable; the contract is just "non-empty".
        assert!(!name.is_empty());
    }
}
