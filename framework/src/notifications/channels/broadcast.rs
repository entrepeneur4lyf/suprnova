//! Broadcast notification channel (stub).
//!
//! Phase 7B will replace this with real WebSocket fan-out. For Phase 5B the
//! channel is wired but its [`Channel::deliver`] implementation only emits
//! a `tracing::info` event — no WebSocket transport exists yet (Phase 7A
//! delivers that). Registering this stub today lets notifications declare
//! `"broadcast"` alongside `"mail"` / `"database"` / `"webpush"` without
//! the dispatcher logging an unregistered-channel warning, and the
//! structured fields on the info event give operators a paper trail of
//! every notification that *would* have been broadcast.

use crate::error::FrameworkError;
use crate::notifications::{Channel, DynNotification};
use async_trait::async_trait;

/// Stub broadcast channel — logs delivery instead of fanning out to
/// WebSocket subscribers. Replaced by a real implementation in Phase 7B
/// once the WebSocket transport (Phase 7A) lands.
#[derive(Default)]
pub struct BroadcastChannelStub;

impl BroadcastChannelStub {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Channel for BroadcastChannelStub {
    fn name(&self) -> &'static str {
        "broadcast"
    }

    async fn deliver(
        &self,
        route: &str,
        notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        tracing::info!(
            channel = "broadcast",
            route = %route,
            notification = %notification.name(),
            data = %notification.data(),
            "broadcast channel stub — WebSocket transport lands in Phase 7B"
        );
        Ok(())
    }
}
