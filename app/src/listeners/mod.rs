//! Application event listeners.
//!
//! Each listener is registered in `bootstrap.rs::register()` and
//! invoked by the framework's dispatcher when an event of the
//! matching type is published. Listeners are concrete types (not
//! closures) so they can be `Arc`'d, named in stack traces, and
//! shared across the dispatcher's storage.

use suprnova::{FrameworkError, async_trait, events::Listener};
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
