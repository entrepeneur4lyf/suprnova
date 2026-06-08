//! Broadcast notification channel.
//!
//! Delivers a notification to WebSocket subscribers by publishing it to the
//! application's [`BroadcastHub`] (Phase 7B). The per-recipient
//! `route_for("broadcast")` value is the broadcast channel name, the
//! notification's type name is the event, and its `data()` is the payload.

use crate::broadcasting::{BroadcastEnvelope, BroadcastHub};
use crate::container::App;
use crate::error::FrameworkError;
use crate::notifications::{Channel, DynNotification};
use async_trait::async_trait;

/// Broadcast channel — publishes notifications to the application's
/// [`BroadcastHub`] so WebSocket subscribers receive them in real time.
///
/// The hub is resolved from the container at delivery time
/// (`App::make::<dyn BroadcastHub>()`), the same way the WS handler and SSE
/// bridge obtain it. Bind one at boot with
/// `App::bind::<dyn BroadcastHub>(Arc::clone(&hub))`.
///
/// # Dispatch semantics (load-bearing — do not "simplify" back to `Ok`)
///
/// [`Channel::deliver`] returns `Err` when **no** `BroadcastHub` is bound in
/// the container. The [`crate::notifications`] dispatcher breaks on the first
/// channel error, so this short-circuits the rest of the notification's
/// channels — **by design**. A notification that declares `"broadcast"` in an
/// app that never wired a hub is a misconfiguration that must surface, not be
/// silently dropped. (This type was previously a stub that returned `Ok(())`
/// without delivering anything; that silent success was the bug being fixed.)
///
/// When a hub **is** bound — the normal case — `deliver` publishes and
/// returns `Ok(())`, so broadcast never short-circuits a correctly-configured
/// app. Publishing to a channel with zero live subscribers is not an error.
#[derive(Default)]
pub struct BroadcastChannel;

impl BroadcastChannel {
    /// Build a new `BroadcastChannel`. Stateless — the bound `BroadcastHub` is resolved per-call.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Channel for BroadcastChannel {
    fn name(&self) -> &'static str {
        "broadcast"
    }

    async fn deliver(
        &self,
        route: &str,
        notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        let hub = App::make::<dyn BroadcastHub>().ok_or_else(|| {
            FrameworkError::internal(
                "broadcast notification channel requires a BroadcastHub bound in the \
                 container — call `App::bind::<dyn BroadcastHub>(Arc::clone(&hub))` at boot, \
                 or drop \"broadcast\" from the notification's channels()",
            )
        })?;

        let envelope = BroadcastEnvelope::new(
            route.to_string(),
            notification.name().to_string(),
            notification.data(),
        );
        tracing::debug!(
            channel = %envelope.channel,
            event = %envelope.event,
            "publishing broadcast notification to hub"
        );
        // Propagate hub publish failures: a cross-process fanout loss
        // is real and the notification dispatcher should surface it,
        // not swallow it.
        hub.publish(envelope).await?;
        Ok(())
    }
}
