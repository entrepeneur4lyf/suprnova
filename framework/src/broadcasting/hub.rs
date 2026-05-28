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
use tokio::sync::RwLock as AsyncRwLock;
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
    /// Optional connection `socket_id` to exclude from delivery — the basis
    /// for [`Broadcastable::broadcast_to_others`](crate::broadcasting::Broadcastable::broadcast_to_others).
    /// The forwarder for the matching connection skips this envelope; every
    /// other subscriber still receives it. `None` (the default) delivers to
    /// all. It rides the cross-process fanout so exclusion holds there too, but
    /// is never forwarded to clients — it is a server-side routing concern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub except: Option<String>,
}

impl BroadcastEnvelope {
    /// A new envelope delivered to every subscriber (no exclusion).
    pub fn new(channel: impl Into<String>, event: impl Into<String>, data: Value) -> Self {
        Self {
            channel: channel.into(),
            event: event.into(),
            data,
            except: None,
        }
    }

    /// Exclude one connection (by its `socket_id`) from this broadcast. Used by
    /// `broadcast_to_others`, and the per-dispatch escape hatch for code that
    /// publishes directly: `hub.publish(BroadcastEnvelope::new(c, e, d).with_except(id))`.
    pub fn with_except(mut self, socket_id: impl Into<String>) -> Self {
        self.except = Some(socket_id.into());
        self
    }
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
        //
        // Domain 16 audit D16-A — was
        // `lock::write(...).expect("BroadcastHub channels RwLock poisoned")`
        // which violates the framework's own `framework/src/lock.rs`
        // policy ("treat a poisoned lock as an internal error,
        // callers should almost always get a FrameworkError instead
        // of panicking"). The lock SHOULD be poison-immune in practice
        // (no panic-able code runs under the write guard), but we
        // route through the helper to make the path consistent with
        // the rest of the framework.
        //
        // On poison: log an error and return an orphan sender (one
        // with no live receivers). Publishes to it succeed-and-drop;
        // subscribes get a Receiver that will never see a message.
        // This isolates the failure to broadcasting (messages silently
        // dropped + visible in logs) instead of taking down the
        // request via panic.
        match lock::write(&self.channels) {
            Ok(mut map) => map
                .entry(channel.to_string())
                .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
                .clone(),
            Err(_) => {
                tracing::error!(
                    channel = %channel,
                    "BroadcastHub channels RwLock poisoned; returning orphan \
                     sender for this channel. Messages on this channel will \
                     be dropped silently until process restart."
                );
                broadcast::channel(CHANNEL_CAPACITY).0
            }
        }
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
        hub.publish(BroadcastEnvelope::new("lonely", "Tick", json!({})))
            .await;
    }
}
