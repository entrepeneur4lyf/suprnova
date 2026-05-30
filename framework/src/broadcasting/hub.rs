//! `BroadcastHub` + the default in-process implementation backed by
//! `tokio::sync::broadcast` per channel.
//!
//! Channels are created lazily on first subscribe or publish.
//! Subscriber drops are reflected in `subscriber_count` after the
//! next publish operation, per `tokio::sync::broadcast` semantics.
//!
//! Long-running processes that publish to a churn of distinct channel
//! names (e.g. `user.{id}.orders`) would otherwise leak a sender per
//! channel even after every subscriber detached. The in-memory hub
//! piggy-backs an opportunistic sweep onto every new-channel creation
//! to evict senders whose `receiver_count() == 0`. This bounds growth
//! to the live working set without a background task.

use crate::FrameworkError;
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
    ///
    /// Returning `Ok(())` with zero local subscribers is normal — channels
    /// are created on demand. `Err(_)` signals that the publish could not
    /// be reliably delivered: for in-process hubs that's effectively never
    /// (`Ok(())` always), but for cross-process implementations it covers
    /// fanout backend failures (broker disconnect, stream closed) so the
    /// caller — typically [`crate::events::Event::dispatch`] via
    /// [`BroadcastListener`](crate::broadcasting::BroadcastListener) — can
    /// surface the loss instead of swallowing it. Local fanout, when it
    /// happens, runs before the error is returned and is unaffected.
    async fn publish(&self, envelope: BroadcastEnvelope) -> Result<(), FrameworkError>;

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

    /// Subscribe to `channel`, evicting any zero-receiver senders we
    /// pass through on the way. The returned receiver is created **before**
    /// any map lock is released, so a concurrent eviction sweep can never
    /// observe a receiver_count == 0 in a race window between sender clone
    /// and the caller calling `.subscribe()` on it. New-channel creation
    /// uses the write lock the prune path also needs, making this
    /// atomic w.r.t. eviction.
    fn subscribe_for(&self, channel: &str) -> broadcast::Receiver<BroadcastEnvelope> {
        // Fast path — read lock, sender already exists. Subscribe under
        // the read guard so the sender can't be swept between clone and
        // subscribe.
        if let Ok(map) = self.channels.read()
            && let Some(tx) = map.get(channel)
        {
            return tx.subscribe();
        }
        // Slow path — create the channel under a write lock and sweep
        // any dead siblings while we hold it.
        match lock::write(&self.channels, "broadcast hub channels") {
            Ok(mut map) => {
                let tx = map
                    .entry(channel.to_string())
                    .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0);
                let rx = tx.subscribe();
                Self::sweep_dead_channels(&mut map, channel);
                rx
            }
            Err(_) => {
                tracing::error!(
                    channel = %channel,
                    "BroadcastHub channels RwLock poisoned; returning orphan \
                     receiver for this channel. Messages on this channel will \
                     never arrive until process restart."
                );
                broadcast::channel(CHANNEL_CAPACITY).0.subscribe()
            }
        }
    }

    /// Resolve the sender for a publish. Unlike `subscribe_for`, this
    /// path never creates the channel — publishing to a channel with no
    /// subscribers is a deliberate no-op (the alternative — creating an
    /// orphan sender per publish — is the leak we're trying to avoid).
    fn sender_for_publish(&self, channel: &str) -> Option<broadcast::Sender<BroadcastEnvelope>> {
        self.channels.read().ok()?.get(channel).cloned()
    }

    /// Evict zero-receiver senders. Called under the write lock so the
    /// observation is consistent with subscribe (no receiver can appear
    /// between `receiver_count() == 0` and the removal). The channel
    /// being created right now is preserved unconditionally: it currently
    /// has the receiver we just minted but the caller hasn't observed
    /// it yet, so its `receiver_count` could read as 0 transiently.
    fn sweep_dead_channels(
        map: &mut HashMap<String, broadcast::Sender<BroadcastEnvelope>>,
        keep: &str,
    ) {
        map.retain(|k, tx| k == keep || tx.receiver_count() > 0);
    }

    /// Number of distinct channels currently held in the map — both live
    /// and (until the next sweep) idle. Exposed for tests that exercise
    /// the eviction policy; `subscriber_count` can't distinguish "no
    /// channel" from "channel exists with 0 receivers".
    #[cfg(test)]
    pub(crate) fn channel_count(&self) -> usize {
        self.channels.read().map(|m| m.len()).unwrap_or(0)
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
        self.subscribe_for(channel)
    }

    async fn publish(&self, envelope: BroadcastEnvelope) -> Result<(), FrameworkError> {
        if let Some(sender) = self.sender_for_publish(&envelope.channel) {
            // send() Errs only when there are no subscribers; that's
            // not an error from the publisher's perspective.
            let _ = sender.send(envelope);
        }
        // No-subscriber publishes are normal: in-process delivery has
        // nowhere to go and that isn't a failure.
        Ok(())
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
        // Should not panic, should not log error, and returns Ok.
        hub.publish(BroadcastEnvelope::new("lonely", "Tick", json!({})))
            .await
            .expect("in-memory publish is infallible");
    }

    #[tokio::test]
    async fn publish_without_subscribers_does_not_create_channel() {
        // Publishes to never-subscribed names must not park senders
        // forever — that was the original growth vector (`user.{id}` etc.).
        let hub = InMemoryBroadcastHub::new();
        for i in 0..100 {
            hub.publish(BroadcastEnvelope::new(
                format!("user.{i}.orders"),
                "Tick",
                json!({}),
            ))
            .await
            .unwrap();
        }
        assert_eq!(hub.channel_count(), 0);
    }

    #[tokio::test]
    async fn new_channel_creation_sweeps_dead_siblings() {
        let hub = InMemoryBroadcastHub::new();
        // Spin up two live channels.
        let _a = hub.subscribe("user.1.orders");
        let _b = hub.subscribe("user.2.orders");
        // And a transient one that loses its only receiver.
        {
            let _gone = hub.subscribe("user.3.orders");
        }
        assert_eq!(hub.channel_count(), 3);

        // Force the slow path (write-lock + sweep) by subscribing to a
        // new channel.
        let _c = hub.subscribe("user.4.orders");

        // Three live (1, 2, 4); user.3 evicted.
        assert_eq!(hub.channel_count(), 3);
    }

    #[tokio::test]
    async fn live_subscriber_survives_concurrent_eviction_sweep() {
        // Regression guard: a fast-path subscribe must hand back a
        // Receiver minted **before** the read lock is released. If the
        // sender were cloned and the lock dropped, a concurrent sweep
        // could see receiver_count == 0 and evict before .subscribe()
        // runs — the new subscriber would silently miss every event.
        let hub = Arc::new(InMemoryBroadcastHub::new());

        let live = hub.subscribe("hot.chan");
        // `live` is a Receiver, sender stays in the map. Now exercise
        // the slow-path sweep over and over via a churn channel while
        // a second subscriber races in on the hot channel.
        let racer = Arc::clone(&hub);
        let race = tokio::spawn(async move {
            // Subscribe under the read-lock fast path.
            let mut rx = racer.subscribe("hot.chan");
            // Publish from the same task — the receiver was created
            // under the read guard so it MUST see the event.
            racer
                .publish(BroadcastEnvelope::new("hot.chan", "Ping", json!({})))
                .await
                .unwrap();
            rx.recv().await.expect("racer receives despite sweep")
        });

        // Generate sweep pressure by creating-and-dropping siblings.
        for i in 0..16 {
            let _ = hub.subscribe(&format!("sweep.{i}"));
        }
        let got = race.await.unwrap();
        assert_eq!(got.event, "Ping");
        drop(live);
    }
}
