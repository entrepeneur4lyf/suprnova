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
