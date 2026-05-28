//! `Broadcastable: Event + Serialize` — events that get pushed to
//! WebSocket subscribers in addition to running in-process Listeners.
//!
//! User code opts in by:
//!   1. Implementing `Event` (existing system) + `Serialize` + `Broadcastable`
//!   2. Calling `EventFacade::broadcast::<E>(hub).await` at boot once
//!      per Broadcastable type
//!
//! When the event is later dispatched via `EventFacade::dispatch(event)`,
//! the framework runs all in-process Listeners (existing behavior) AND
//! publishes the event's JSON serialization on every channel named by
//! `broadcast_on(&self)`.

use crate::FrameworkError;
use crate::broadcasting::hub::{BroadcastEnvelope, BroadcastHub};
use crate::events::{Event, Listener};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::marker::PhantomData;
use std::sync::Arc;

/// Marker trait for events that get pushed to WebSocket subscribers.
///
/// Implementers must list the channel names the event broadcasts on.
/// The channels can be parameterized by event fields (e.g.,
/// `format!("user.{}.orders", self.user_id)`).
pub trait Broadcastable: Event + Serialize {
    /// Channel names this event broadcasts on when dispatched.
    /// Called once per dispatch; cheap allocation is fine.
    fn broadcast_on(&self) -> Vec<String>;

    /// Event name as it appears on the wire (in the `event` field
    /// of the ServerFrame::Event envelope). Default: `Event::event_name()`.
    fn broadcast_event_name(&self) -> &'static str {
        Self::event_name()
    }

    /// The payload pushed to subscribers. `None` (the default) serializes the
    /// whole event via [`Serialize`]; return `Some(value)` to broadcast a
    /// curated shape instead — Laravel's `broadcastWith()`. The broadcast
    /// payload is independent of the event's own fields, so you can omit
    /// secrets or reshape for the client without changing the event type.
    fn broadcast_with(&self) -> Option<Value> {
        None
    }

    /// Whether to broadcast *this* instance. `true` by default. Returning
    /// `false` dispatches the event to in-process [`Listener`]s as usual but
    /// suppresses the WebSocket push — Laravel's `broadcastWhen()`. Only the
    /// broadcast is skipped; the rest of the event pipeline is unaffected.
    fn broadcast_when(&self) -> bool {
        true
    }
}

/// Generic Listener that publishes the event to the hub when fired.
/// Registered via `EventFacade::broadcast::<E>(hub)`.
pub struct BroadcastListener<E: Broadcastable> {
    hub: Arc<dyn BroadcastHub>,
    _marker: PhantomData<E>,
}

impl<E: Broadcastable> BroadcastListener<E> {
    pub fn new(hub: Arc<dyn BroadcastHub>) -> Self {
        Self {
            hub,
            _marker: PhantomData,
        }
    }
}

#[async_trait]
impl<E: Broadcastable> Listener<E> for BroadcastListener<E> {
    async fn handle(&self, event: &E) -> Result<(), FrameworkError> {
        // `broadcast_when() == false` suppresses only the WS push — by the time
        // this listener runs, the event has already reached its in-process
        // listeners; we just skip publishing to the hub.
        if !event.broadcast_when() {
            return Ok(());
        }
        let channels = event.broadcast_on();
        if channels.is_empty() {
            return Ok(());
        }
        // `broadcast_with()` chooses the wire payload: a curated value when
        // provided, otherwise the event's full `Serialize` form.
        let data = match event.broadcast_with() {
            Some(custom) => custom,
            None => serde_json::to_value(event).map_err(|e| {
                FrameworkError::internal(format!("Broadcastable serde failed: {e}"))
            })?,
        };
        let event_name = event.broadcast_event_name().to_string();
        for channel in channels {
            self.hub
                .publish(BroadcastEnvelope {
                    channel,
                    event: event_name.clone(),
                    data: data.clone(),
                })
                .await;
        }
        Ok(())
    }
}
