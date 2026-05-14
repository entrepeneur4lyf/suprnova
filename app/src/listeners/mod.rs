//! Application event listeners.
//!
//! Each listener is registered in `bootstrap.rs::register()` and
//! invoked by the framework's dispatcher when an event of the
//! matching type is published. Listeners are concrete types (not
//! closures) so they can be `Arc`'d, named in stack traces, and
//! shared across the dispatcher's storage.

use std::sync::Arc;
use suprnova::{async_trait, events::Listener, FrameworkError};
use tokio::sync::broadcast;
use tracing::info;

use crate::events::UserRegistered;

/// Logs a synthetic "welcome email sent" line. Real `Mail`-backed
/// delivery lands in Phase 5 (queue/mail/notifications); the
/// listener exists today to dogfood the event surface and prove
/// the dispatcher routes events to typed listeners.
pub struct SendWelcomeEmailListener;

#[async_trait]
impl Listener<UserRegistered> for SendWelcomeEmailListener {
    async fn handle(&self, event: &UserRegistered) -> Result<(), FrameworkError> {
        info!(
            user_id = event.user_id,
            email = %event.email,
            "would send welcome email"
        );
        Ok(())
    }
}

/// Forwards every dispatched `UserRegistered` event into a tokio
/// `broadcast::Sender`. The `/events/stream` SSE handler holds the
/// receiver side, so any controller that dispatches the event will
/// fan out to every connected SSE client live.
///
/// Holding `Arc<broadcast::Sender<_>>` keeps the sender alive for
/// the lifetime of the listener; if the channel has no live
/// receivers, `send` returns `SendError`, which we deliberately
/// swallow — the event still ran through the dispatcher and the
/// other listeners (the welcome-email logger) processed it normally.
pub struct UserRegisteredBroadcaster {
    sender: Arc<broadcast::Sender<UserRegistered>>,
}

impl UserRegisteredBroadcaster {
    pub fn new(sender: Arc<broadcast::Sender<UserRegistered>>) -> Self {
        Self { sender }
    }
}

#[async_trait]
impl Listener<UserRegistered> for UserRegisteredBroadcaster {
    async fn handle(&self, event: &UserRegistered) -> Result<(), FrameworkError> {
        // `send` only errors when there are no receivers. That's
        // fine — it means no one's listening on /events/stream right
        // now; the event still got dispatched to other listeners.
        let _ = self.sender.send(event.clone());
        Ok(())
    }
}
