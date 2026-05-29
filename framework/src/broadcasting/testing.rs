//! Test doubles for broadcasting — the Suprnova analogue of Laravel's
//! `Broadcast::fake()` + `assertBroadcasted`.
//!
//! [`RecordingBroadcastHub`] is a [`BroadcastHub`] that records every published
//! envelope for assertions while still delivering to live subscribers. Bind it
//! in place of [`InMemoryBroadcastHub`] in a test and assert what was broadcast
//! without subscribing first. Available to consumer-crate tests by default (no
//! feature gate), like `Event::fake`.

use super::hub::{BroadcastEnvelope, BroadcastHub, InMemoryBroadcastHub};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Mutex;
use tokio::sync::broadcast;

/// A recording [`BroadcastHub`] for tests. Wraps an in-process hub so
/// subscribe / publish / presence behave exactly as in production, and
/// additionally records every published [`BroadcastEnvelope`] for assertions.
///
/// ```rust,ignore
/// use suprnova::broadcasting::RecordingBroadcastHub;
///
/// let hub = RecordingBroadcastHub::new();
/// // ... run code that dispatches a broadcast against `hub` ...
/// hub.assert_broadcast("orders.42", "OrderShipped");
/// ```
pub struct RecordingBroadcastHub {
    inner: InMemoryBroadcastHub,
    published: Mutex<Vec<BroadcastEnvelope>>,
}

impl RecordingBroadcastHub {
    /// A fresh recording hub with nothing published yet.
    pub fn new() -> Self {
        Self {
            inner: InMemoryBroadcastHub::new(),
            published: Mutex::new(Vec::new()),
        }
    }

    /// Every envelope published so far, in publish order. Poison-safe (returns
    /// an empty vec if the lock was poisoned, never panics).
    pub fn broadcasts(&self) -> Vec<BroadcastEnvelope> {
        self.published.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Number of envelopes published so far.
    pub fn count(&self) -> usize {
        self.published.lock().map(|v| v.len()).unwrap_or(0)
    }

    /// Panic unless an `event` was broadcast on `channel`.
    pub fn assert_broadcast(&self, channel: &str, event: &str) {
        let recorded = self.broadcasts();
        let found = recorded
            .iter()
            .any(|e| e.channel == channel && e.event == event);
        assert!(
            found,
            "expected a broadcast of `{event}` on `{channel}`, but none was recorded. \
             Recorded: {:?}",
            recorded
                .iter()
                .map(|e| (e.channel.as_str(), e.event.as_str()))
                .collect::<Vec<_>>()
        );
    }

    /// Panic if anything was broadcast.
    pub fn assert_nothing_broadcast(&self) {
        let n = self.count();
        assert_eq!(n, 0, "expected no broadcasts, but {n} were recorded");
    }
}

impl Default for RecordingBroadcastHub {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BroadcastHub for RecordingBroadcastHub {
    fn subscribe(&self, channel: &str) -> broadcast::Receiver<BroadcastEnvelope> {
        self.inner.subscribe(channel)
    }

    async fn publish(&self, envelope: BroadcastEnvelope) {
        if let Ok(mut v) = self.published.lock() {
            v.push(envelope.clone());
        }
        self.inner.publish(envelope).await;
    }

    fn subscriber_count(&self, channel: &str) -> usize {
        self.inner.subscriber_count(channel)
    }

    async fn track_member(&self, channel: &str, member_id: &str, info: Value) {
        self.inner.track_member(channel, member_id, info).await;
    }

    async fn untrack_member(&self, channel: &str, member_id: &str) {
        self.inner.untrack_member(channel, member_id).await;
    }

    async fn list_members(&self, channel: &str) -> Vec<Value> {
        self.inner.list_members(channel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn records_and_asserts_broadcasts() {
        let hub = RecordingBroadcastHub::new();
        hub.assert_nothing_broadcast();

        hub.publish(BroadcastEnvelope::new(
            "chat.1",
            "Msg",
            json!({ "t": "hi" }),
        ))
        .await;

        hub.assert_broadcast("chat.1", "Msg");
        assert_eq!(hub.count(), 1);
        assert_eq!(hub.broadcasts()[0].channel, "chat.1");
    }

    #[tokio::test]
    async fn still_delivers_to_live_subscribers() {
        let hub = RecordingBroadcastHub::new();
        let mut rx = hub.subscribe("chat.1");
        hub.publish(BroadcastEnvelope::new("chat.1", "Msg", json!({})))
            .await;
        let env = rx.recv().await.expect("delivered");
        assert_eq!(env.event, "Msg");
    }

    #[tokio::test]
    #[should_panic(expected = "expected a broadcast")]
    async fn assert_broadcast_panics_when_absent() {
        let hub = RecordingBroadcastHub::new();
        hub.assert_broadcast("nope", "Nope");
    }
}
