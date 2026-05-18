//! `BroadcastHub` + the default in-process implementation backed by
//! `tokio::sync::broadcast` per channel.
//!
//! Channels are created lazily on first subscribe or publish.
//! Subscriber drops are reflected in `subscriber_count` after the
//! next publish operation, per `tokio::sync::broadcast` semantics.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

/// Per-channel broadcast buffer capacity. Subscribers that fall this
/// far behind get a `RecvError::Lagged(n)` on the next recv; the
/// handler decides how to surface it. 256 is comfortable for typical
/// chat/presence workloads.
const CHANNEL_CAPACITY: usize = 256;

/// One published event on a channel.
///
/// Named `BroadcastEnvelope` (rather than `Envelope`) to avoid a
/// naming conflict with `queue::Envelope`, which is also re-exported
/// from the `suprnova` crate root.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BroadcastEnvelope {
    /// Channel name (e.g. "chat.42", "presence.lobby").
    pub channel: String,
    /// Event name (e.g. "MessagePosted", "MemberJoined").
    pub event: String,
    /// Event payload, opaque to the hub.
    pub data: Value,
}

/// The broadcasting primitive. Apps depend on `Arc<dyn BroadcastHub>`
/// resolved from the container so the in-process default can be
/// swapped for a multi-process implementation without touching the
/// publish/subscribe call sites.
#[async_trait]
pub trait BroadcastHub: Send + Sync + 'static {
    /// Subscribe to a channel; the returned receiver yields each
    /// published envelope for as long as it stays in scope. Dropping
    /// the receiver detaches; the slot is reclaimed on the next
    /// publish to that channel.
    fn subscribe(&self, channel: &str) -> broadcast::Receiver<BroadcastEnvelope>;

    /// Publish an envelope to all subscribers of `envelope.channel`.
    /// Returns silently if no subscribers exist — that's not an
    /// error condition (channels are created on demand).
    async fn publish(&self, envelope: BroadcastEnvelope);

    /// Subscriber count for a channel. `0` if no one is subscribed
    /// or the channel hasn't been created yet.
    fn subscriber_count(&self, _channel: &str) -> usize {
        0
    }
}

/// In-process broadcast hub. Default for single-process apps.
pub struct InMemoryBroadcastHub {
    channels: Arc<RwLock<HashMap<String, broadcast::Sender<BroadcastEnvelope>>>>,
}

impl InMemoryBroadcastHub {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    fn sender_for(&self, channel: &str) -> broadcast::Sender<BroadcastEnvelope> {
        // Fast path — read lock for the common case.
        if let Some(tx) = self
            .channels
            .read()
            .ok()
            .and_then(|m| m.get(channel).cloned())
        {
            return tx;
        }
        // Slow path — create the channel under a write lock.
        let mut map = self
            .channels
            .write()
            .expect("BroadcastHub channels RwLock poisoned");
        map.entry(channel.to_string())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .clone()
    }
}

impl Default for InMemoryBroadcastHub {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BroadcastHub for InMemoryBroadcastHub {
    fn subscribe(&self, channel: &str) -> broadcast::Receiver<BroadcastEnvelope> {
        self.sender_for(channel).subscribe()
    }

    async fn publish(&self, envelope: BroadcastEnvelope) {
        let sender = self.sender_for(&envelope.channel);
        // send() Errs only when there are no subscribers; that's
        // not an error from the publisher's perspective.
        let _ = sender.send(envelope);
    }

    fn subscriber_count(&self, channel: &str) -> usize {
        self.channels
            .read()
            .ok()
            .and_then(|m| m.get(channel).map(|tx| tx.receiver_count()))
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn publish_with_no_subscribers_is_silent() {
        let hub = InMemoryBroadcastHub::new();
        // Should not panic, should not log error.
        hub.publish(BroadcastEnvelope {
            channel: "lonely".into(),
            event: "Tick".into(),
            data: json!({}),
        })
        .await;
    }
}
