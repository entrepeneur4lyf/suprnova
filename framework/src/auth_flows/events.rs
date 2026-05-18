//! Phase 11 auth-flow events.
//!
//! The complete catalogue lands in Task 7 — this module grows
//! incrementally as subsequent tasks need additional event types.

use crate::events::Event;

/// Fires when a user successfully verifies their email address via
/// [`crate::auth_flows::EmailVerification::verify`].
///
/// `user_id` is the stringified torii `UserId`, suitable for crossing
/// task / serialization boundaries.
#[derive(Debug, Clone)]
pub struct EmailVerified {
    pub user_id: String,
}

impl Event for EmailVerified {
    fn event_name() -> &'static str {
        "EmailVerified"
    }
}

/// Fires when a user successfully completes a password reset via
/// [`crate::auth_flows::PasswordReset::complete`].
///
/// `user_id` is the stringified torii `UserId`. Listeners typically
/// revoke active sessions, audit-log the event, or trigger
/// supplemental security notifications beyond the built-in
/// [`crate::auth_flows::PasswordChangedMail`].
#[derive(Debug, Clone)]
pub struct PasswordResetCompleted {
    pub user_id: String,
}

impl Event for PasswordResetCompleted {
    fn event_name() -> &'static str {
        "PasswordResetCompleted"
    }
}

/// Fires when an administrator (or another flow such as a successful
/// password reset) forcibly unlocks an account that was previously
/// locked due to too many failed login attempts. See
/// [`crate::auth_flows::BruteForce::unlock_account`].
///
/// The event is **only** emitted when `unlock_account` reports that
/// the account had been locked; a no-op unlock on an already-unlocked
/// email does not fire. Listeners can therefore treat each
/// `AccountUnlocked` as a real security-state transition (audit log,
/// admin notification, etc.).
#[derive(Debug, Clone)]
pub struct AccountUnlocked {
    pub email: String,
}

impl Event for AccountUnlocked {
    fn event_name() -> &'static str {
        "AccountUnlocked"
    }
}

/// Fires when a user successfully confirms 2FA enrollment via
/// [`crate::auth_flows::TwoFactor::confirm`].
///
/// `user_id` is the stringy identifier passed to the
/// [`crate::auth_flows::TwoFactorUser`] contract (typically
/// `torii::UserId.to_string()`). The event fires once per successful
/// confirmation; re-enrolling and re-confirming fires a fresh event.
#[derive(Debug, Clone)]
pub struct TwoFactorEnrolled {
    pub user_id: String,
}

impl Event for TwoFactorEnrolled {
    fn event_name() -> &'static str {
        "TwoFactorEnrolled"
    }
}

/// Fires when 2FA is disabled for a user via
/// [`crate::auth_flows::TwoFactor::disable`].
///
/// **Only** emitted when a real state transition occurs — i.e. a row
/// existed in `two_factor_credentials` and was removed. A no-op
/// disable on a user who never enrolled does not fire, mirroring the
/// [`AccountUnlocked`] contract so audit listeners can treat each
/// `TwoFactorDisabled` as a meaningful security event.
#[derive(Debug, Clone)]
pub struct TwoFactorDisabled {
    pub user_id: String,
}

impl Event for TwoFactorDisabled {
    fn event_name() -> &'static str {
        "TwoFactorDisabled"
    }
}
