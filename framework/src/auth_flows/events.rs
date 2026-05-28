//! Auth-flow events.
//!
//! Seven events covering the security-state transitions emitted by the
//! auth_flows facades:
//!
//! - [`EmailVerified`] — `EmailVerification::verify` consumed a valid token.
//! - [`PasswordResetLinkSent`] — `PasswordReset::request` issued a token
//!   for an on-file email (anti-enumeration: no event fires when the
//!   email is absent).
//! - [`PasswordResetCompleted`] — `PasswordReset::complete` succeeded.
//! - [`AccountLocked`] — `BruteForce::record_failed_attempt` pushed an
//!   account across the threshold (unlocked → locked transition).
//! - [`AccountUnlocked`] — `BruteForce::unlock_account` cleared a real
//!   lock (no-op unlocks on already-unlocked accounts do not fire).
//! - [`TwoFactorEnrolled`] — `TwoFactor::confirm` set `confirmed_at`.
//! - [`TwoFactorDisabled`] — `TwoFactor::disable` removed an existing
//!   2FA row (no-op disables on never-enrolled users do not fire).
//!
//! Every event is `Debug + Clone + 'static`, carries no sensitive data
//! (no plaintext tokens, no IPs), and uses stringy identifiers so
//! listeners can serialize them across task boundaries without leaking
//! type information from the user-storage backend.

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

/// Fires when [`crate::auth_flows::PasswordReset::request`] successfully
/// issues a reset token for an email that is on file.
///
/// **Anti-enumeration:** the event does **not** fire when the email
/// is absent — that path returns `Ok(None)` with no token minted and
/// no side effect, so a listener that counts events cannot
/// distinguish "absent email" from "no request made." Listeners
/// typically audit-log the action (the user just received a sensitive
/// security email) or alert on suspicious patterns (repeated requests
/// against the same user from different peer IPs).
///
/// `user_id` is the stringified torii `UserId`; `email` is the address
/// the reset link was dispatched to (matches the input on file, not
/// necessarily the raw request input, in case torii normalises).
#[derive(Debug, Clone)]
pub struct PasswordResetLinkSent {
    pub user_id: String,
    pub email: String,
}

impl Event for PasswordResetLinkSent {
    fn event_name() -> &'static str {
        "PasswordResetLinkSent"
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

/// Fires when [`crate::auth_flows::BruteForce::record_failed_attempt`]
/// pushes an account across the lockout threshold — the
/// unlocked → locked state transition. Subsequent failed attempts
/// while the account remains locked do not re-fire the event, so
/// listeners can treat each `AccountLocked` as a fresh security
/// incident worth notifying (admin alert, audit log, throttle a peer
/// IP, etc.).
///
/// `failed_attempts` is the count at the moment of lock — useful when
/// the threshold is configurable and the listener wants to log how
/// many attempts triggered this specific lock.
#[derive(Debug, Clone)]
pub struct AccountLocked {
    pub email: String,
    pub failed_attempts: u32,
}

impl Event for AccountLocked {
    fn event_name() -> &'static str {
        "AccountLocked"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names_distinct() {
        let mut names = vec![
            EmailVerified::event_name(),
            PasswordResetLinkSent::event_name(),
            PasswordResetCompleted::event_name(),
            AccountLocked::event_name(),
            AccountUnlocked::event_name(),
            TwoFactorEnrolled::event_name(),
            TwoFactorDisabled::event_name(),
        ];
        let before = names.len();
        names.sort();
        names.dedup();
        assert_eq!(
            names.len(),
            before,
            "duplicate event_name() across auth_flows events: {names:?}"
        );
    }
}
