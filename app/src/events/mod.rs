//! Application events.
//!
//! Defined here, registered with listeners in `bootstrap.rs`, and
//! dispatched from controllers/actions via
//! `suprnova::EventFacade::dispatch(...)`. Acts as our dogfood for
//! the framework's event surface (Phase 1) and as the integration
//! point that the `/events/stream` SSE handler subscribes to.

use suprnova::broadcasting::Broadcastable;
use suprnova::Event;

/// Fired when a new user finishes registration. Carries enough
/// identity to drive welcome emails, audit trails, and the SSE
/// activity feed without needing a database round-trip on the
/// listener side.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UserRegistered {
    /// Database id assigned to the new user.
    pub user_id: i64,
    /// Email captured at registration. Stored verbatim — the listener
    /// is responsible for any normalization it needs.
    pub email: String,
}

impl Event for UserRegistered {
    fn event_name() -> &'static str {
        "UserRegistered"
    }
}

impl Broadcastable for UserRegistered {
    fn broadcast_on(&self) -> Vec<String> {
        vec!["user_registered".to_string()]
    }
}
