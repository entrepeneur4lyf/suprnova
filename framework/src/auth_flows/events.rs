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
