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
///
/// # Multi-channel publish semantics
///
/// When `broadcast_on` returns multiple channels, [`BroadcastListener`]
/// publishes to **every** channel even if one fails — a single broker hiccup
/// on channel N does not skip delivery to channels N+1..K. The first failure
/// is remembered and returned at the end; subsequent failures are logged at
/// `warn`. Local in-process subscribers on channels that did succeed keep
/// their delivery either way.
///
/// # Dispatch ordering with sibling listeners
///
/// The dispatcher used by `Event::dispatch` is **fail-fast**:
/// if a hub publish fails (e.g. broker disconnect on a multi-process backend),
/// the [`BroadcastListener`] returns `Err` and sibling listeners that come
/// after it in the registration order do **not** run. Register the
/// [`BroadcastListener`] **after** in-process listeners whose side effects
/// (DB writes, log emission) must run regardless of broadcast outcome, or
/// switch to `Event::dispatch_best_effort` when you need all
/// listeners to run even if one returns `Err`.
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

    /// Whether to exclude the connection that triggered this broadcast —
    /// the originating WebSocket `socket_id`, taken from the current request's
    /// `X-Socket-ID` header (Laravel's `toOthers()`). `false` by default (every
    /// subscriber receives it). When `true`, the originating connection is
    /// skipped *if one is identified*; off-request (a worker or job) or when the
    /// client sent no `X-Socket-ID`, it degrades to broadcasting to everyone.
    ///
    /// This is a per-event-type choice. For per-dispatch exclusion, publish
    /// directly with
    /// [`BroadcastEnvelope::with_except`](crate::broadcasting::BroadcastEnvelope::with_except).
    fn broadcast_to_others(&self) -> bool {
        false
    }
}

/// Generic Listener that publishes the event to the hub when fired.
/// Registered via `EventFacade::broadcast::<E>(hub)`.
pub struct BroadcastListener<E: Broadcastable> {
    hub: Arc<dyn BroadcastHub>,
    _marker: PhantomData<E>,
}

impl<E: Broadcastable> BroadcastListener<E> {
    /// Construct a listener that publishes every `E` it receives to `hub`.
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
        // `broadcast_to_others()` excludes the connection that triggered this
        // broadcast (the current request's `X-Socket-ID`), when one is present.
        let except = if event.broadcast_to_others() {
            crate::broadcasting::request_socket::current()
        } else {
            None
        };
        // Attempt every channel even if one fails. A `?` short-circuit here
        // would mean a broker error on channel N silently skips local AND
        // remote delivery to channels N+1..K — those subscribers would never
        // observe an event the application believed was dispatched. Instead
        // we publish through the full list, remember the first error, and
        // log any subsequent ones at warn. The first error is then returned
        // so EventFacade::dispatch still surfaces fanout loss to the caller.
        let mut first_error: Option<FrameworkError> = None;
        for channel in channels {
            let mut envelope =
                BroadcastEnvelope::new(channel.clone(), event_name.clone(), data.clone());
            envelope.except = except.clone();
            if let Err(e) = self.hub.publish(envelope).await {
                if first_error.is_none() {
                    first_error = Some(e);
                } else {
                    tracing::warn!(
                        channel = %channel,
                        event = %event_name,
                        error = %e,
                        "BroadcastListener: publish failed on additional channel \
                         after an earlier failure; first error will be returned"
                    );
                }
            }
        }
        if let Some(err) = first_error {
            return Err(err);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcasting::hub::{BroadcastEnvelope, BroadcastHub};
    use crate::events::Event;
    use serde::Serialize;
    use serde_json::json;
    use std::collections::HashSet;
    use std::sync::Mutex;
    use tokio::sync::broadcast;

    /// A hub that records every publish attempt and can be configured to
    /// fail on a chosen set of channels. Used to prove the multi-channel
    /// loop continues past a failure instead of short-circuiting.
    struct PartialFailureHub {
        attempted: Mutex<Vec<String>>,
        delivered: Mutex<Vec<String>>,
        fail_on: HashSet<String>,
    }

    impl PartialFailureHub {
        fn new(fail_on: impl IntoIterator<Item = &'static str>) -> Self {
            Self {
                attempted: Mutex::new(Vec::new()),
                delivered: Mutex::new(Vec::new()),
                fail_on: fail_on.into_iter().map(str::to_string).collect(),
            }
        }
    }

    #[async_trait]
    impl BroadcastHub for PartialFailureHub {
        fn subscribe(&self, _channel: &str) -> broadcast::Receiver<BroadcastEnvelope> {
            // Tests don't subscribe; mint an orphan receiver and drop it.
            broadcast::channel(1).0.subscribe()
        }

        async fn publish(&self, envelope: BroadcastEnvelope) -> Result<(), FrameworkError> {
            if let Ok(mut v) = self.attempted.lock() {
                v.push(envelope.channel.clone());
            }
            if self.fail_on.contains(&envelope.channel) {
                return Err(FrameworkError::internal(format!(
                    "synthetic broker failure on '{}'",
                    envelope.channel
                )));
            }
            if let Ok(mut v) = self.delivered.lock() {
                v.push(envelope.channel.clone());
            }
            Ok(())
        }
    }

    #[derive(Serialize, Clone, Debug)]
    struct MultiChannelEvent {
        channels: Vec<String>,
    }

    impl Event for MultiChannelEvent {
        fn event_name() -> &'static str {
            "MultiChannelEvent"
        }
    }

    impl Broadcastable for MultiChannelEvent {
        fn broadcast_on(&self) -> Vec<String> {
            self.channels.clone()
        }
    }

    #[tokio::test]
    async fn multi_channel_publish_attempts_every_channel_after_failure() {
        // Channel "b" is configured to fail. Without the fix the listener
        // would `?` out on "b" and never try "c" / "d"; with the fix every
        // channel is attempted and "c" / "d" still receive the event.
        let hub = Arc::new(PartialFailureHub::new(["b"]));
        let listener: BroadcastListener<MultiChannelEvent> =
            BroadcastListener::new(hub.clone() as Arc<dyn BroadcastHub>);
        let event = MultiChannelEvent {
            channels: vec!["a".into(), "b".into(), "c".into(), "d".into()],
        };

        let result = listener.handle(&event).await;
        assert!(
            result.is_err(),
            "first failure must still surface as Err so the dispatcher can react"
        );

        let attempted = hub.attempted.lock().unwrap().clone();
        assert_eq!(
            attempted,
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string()
            ],
            "every channel must be attempted regardless of mid-loop failures"
        );

        let delivered = hub.delivered.lock().unwrap().clone();
        assert_eq!(
            delivered,
            vec!["a".to_string(), "c".to_string(), "d".to_string()],
            "channels other than the failing one must still see successful delivery"
        );
    }

    #[tokio::test]
    async fn multi_channel_publish_returns_first_error_when_multiple_fail() {
        // Channels "b" and "d" both fail. The first encountered error
        // must be the one returned; the second is logged at warn.
        let hub = Arc::new(PartialFailureHub::new(["b", "d"]));
        let listener: BroadcastListener<MultiChannelEvent> =
            BroadcastListener::new(hub.clone() as Arc<dyn BroadcastHub>);
        let event = MultiChannelEvent {
            channels: vec!["a".into(), "b".into(), "c".into(), "d".into()],
        };

        let err = listener
            .handle(&event)
            .await
            .expect_err("at least one failure must surface");
        let msg = err.to_string();
        assert!(
            msg.contains("'b'"),
            "expected the first failure ('b') to be the returned error, got: {msg}"
        );

        let attempted = hub.attempted.lock().unwrap().clone();
        assert_eq!(attempted.len(), 4, "every channel must still be attempted");
    }

    #[tokio::test]
    async fn single_channel_failure_still_surfaces_as_err() {
        // Regression guard for the easy case — a single channel that fails
        // must still return Err so the existing semantic for one-channel
        // dispatches doesn't drift.
        let hub = Arc::new(PartialFailureHub::new(["only"]));
        let listener: BroadcastListener<MultiChannelEvent> =
            BroadcastListener::new(hub.clone() as Arc<dyn BroadcastHub>);
        let event = MultiChannelEvent {
            channels: vec!["only".into()],
        };

        let result = listener.handle(&event).await;
        assert!(result.is_err(), "single-channel failure must still error");
    }

    #[derive(Serialize, Clone, Debug)]
    struct OptOutEvent;

    impl Event for OptOutEvent {
        fn event_name() -> &'static str {
            "OptOutEvent"
        }
    }

    impl Broadcastable for OptOutEvent {
        fn broadcast_on(&self) -> Vec<String> {
            vec!["any".into()]
        }
        fn broadcast_when(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn broadcast_when_false_skips_publish_entirely() {
        // The `broadcast_when() == false` early return predates this fix —
        // assert it still wins so we don't burn cycles building envelopes
        // for events that are explicitly not going on the wire.
        let hub = Arc::new(PartialFailureHub::new(["any"]));
        let listener: BroadcastListener<OptOutEvent> =
            BroadcastListener::new(hub.clone() as Arc<dyn BroadcastHub>);
        let result = listener.handle(&OptOutEvent).await;
        assert!(result.is_ok(), "broadcast_when false must return Ok");
        assert!(
            hub.attempted.lock().unwrap().is_empty(),
            "broadcast_when false must not attempt any publish"
        );
    }

    #[derive(Serialize, Clone, Debug)]
    struct ZeroChannelEvent;

    impl Event for ZeroChannelEvent {
        fn event_name() -> &'static str {
            "ZeroChannelEvent"
        }
    }

    impl Broadcastable for ZeroChannelEvent {
        fn broadcast_on(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn empty_channel_list_is_ok_without_touching_hub() {
        // The empty-channels early return also predates this fix; assert
        // we don't accidentally walk through the new loop with zero
        // iterations and synthesise an error.
        let hub = Arc::new(PartialFailureHub::new(Vec::<&'static str>::new()));
        let listener: BroadcastListener<ZeroChannelEvent> =
            BroadcastListener::new(hub.clone() as Arc<dyn BroadcastHub>);
        let _ = json!({}); // keep json! reachable
        let result = listener.handle(&ZeroChannelEvent).await;
        assert!(result.is_ok(), "empty channels must return Ok");
        assert!(
            hub.attempted.lock().unwrap().is_empty(),
            "empty channels must not touch the hub"
        );
    }
}
