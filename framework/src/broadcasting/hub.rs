//! `BroadcastHub` + the default in-process implementation backed by
//! `tokio::sync::broadcast` per channel.
//!
//! Channels are created lazily on first subscribe or publish.
//! Subscriber drops are reflected in `subscriber_count` after the
//! next publish operation, per `tokio::sync::broadcast` semantics.

use crate::lock;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;
use tokio::sync::RwLock as AsyncRwLock;

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

    /// Track a presence member on a channel. The `member_id` is a
    /// per-connection UUID assigned by the handler; `info` is the
    /// public payload returned by [`PresenceChannel::member_info`].
    ///
    /// Default: no-op, so future fanout implementations (T11) can
    /// implement member tracking lazily.
    async fn track_member(&self, _channel: &str, _member_id: &str, _info: Value) {}

    /// Remove a previously tracked member. Called on unsubscribe or
    /// connection close. Default: no-op.
    async fn untrack_member(&self, _channel: &str, _member_id: &str) {}

    /// Return the current member list for a channel — each element is
    /// the `info` value passed to [`track_member`] for a live member.
    /// Default: empty vec.
    async fn list_members(&self, _channel: &str) -> Vec<Value> {
        Vec::new()
    }
}

/// In-process broadcast hub. Default for single-process apps.
pub struct InMemoryBroadcastHub {
    channels: Arc<RwLock<HashMap<String, broadcast::Sender<BroadcastEnvelope>>>>,
    /// Per-channel presence members: channel → (member_id → info).
    members: Arc<AsyncRwLock<HashMap<String, HashMap<String, Value>>>>,
}

impl InMemoryBroadcastHub {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
            members: Arc::new(AsyncRwLock::new(HashMap::new())),
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
        let mut map = lock::write(&self.channels)
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

    async fn track_member(&self, channel: &str, member_id: &str, info: Value) {
        let mut map = self.members.write().await;
        map.entry(channel.to_string())
            .or_default()
            .insert(member_id.to_string(), info);
    }

    async fn untrack_member(&self, channel: &str, member_id: &str) {
        let mut map = self.members.write().await;
        if let Some(ch_members) = map.get_mut(channel) {
            ch_members.remove(member_id);
        }
    }

    async fn list_members(&self, channel: &str) -> Vec<Value> {
        let map = self.members.read().await;
        map.get(channel)
            .map(|ch| ch.values().cloned().collect())
            .unwrap_or_default()
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
