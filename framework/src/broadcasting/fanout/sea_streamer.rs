//! sea-streamer-backed BroadcastHub for cross-process fanout.
//!
//! Wraps an `InMemoryBroadcastHub` for local subscribers AND writes every
//! published envelope to a sea-streamer stream so other processes subscribed
//! to the same stream receive the event in their own local hubs.
//!
//! ## Architecture
//!
//! ```text
//!  publish(envelope)
//!       │
//!       ├─► InMemoryBroadcastHub::publish (immediate, local WS subs)
//!       │
//!       └─► StdioProducer::send (serialized JSON to sea-streamer stream)
//!                │
//!                ▼ (loopback/other processes)
//!           consumer_pump_task
//!                │
//!                ▼ (skip own messages via instance_id)
//!           InMemoryBroadcastHub::publish (cross-process events)
//! ```
//!
//! ## Duplicate-delivery guard
//!
//! Each hub instance is assigned a random `Uuid` on construction. Envelopes
//! are wrapped in a `TaggedEnvelope` that carries the origin `instance_id`.
//! The consumer pump drops messages whose `instance_id` matches the local
//! hub's own ID — this prevents local subscribers from seeing each event
//! twice (once from the direct local publish and once looped back through
//! the stream).
//!
//! ## Backends
//!
//! The implementation uses `sea_streamer::stdio::StdioStreamer` (stdin/stdout
//! pipes). For production, swap in `sea_streamer_kafka::KafkaStreamer` or
//! `sea_streamer_redis::RedisStreamer` — they all implement the same
//! `sea_streamer_types::Streamer` trait. The URI scheme drives the backend
//! when using the socket adapter (`sea-streamer` with the `socket` feature).
//!
//! ## Cross-process presence (v1 scope)
//!
//! `track_member` / `untrack_member` / `list_members` delegate to the local
//! hub for v1. Members registered in other processes are NOT visible locally.
//! Cross-process presence requires a shared store (Redis sorted set, DB) and
//! is a 7B+ follow-on.
//!
//! ## Loopback mode
//!
//! `SeaStreamerBroadcastHub::new_loopback` enables the stdio loopback option,
//! which feeds produced messages back to consumers in the same process. This
//! is intended for testing only — the duplicate guard (instance_id) ensures
//! the local hub still sees each event only once.

use crate::broadcasting::hub::{BroadcastEnvelope, BroadcastHub, InMemoryBroadcastHub};
use crate::FrameworkError;
use async_trait::async_trait;
use sea_streamer::stdio::{
    StdioConnectOptions, StdioConsumerOptions, StdioProducerOptions, StdioStreamer,
};
use sea_streamer::{
    Buffer, Consumer, ConsumerMode, ConsumerOptions, Message, Producer, StreamKey, Streamer,
    StreamerUri,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str::FromStr;
use std::sync::Arc;
use tokio::task::JoinHandle;
use uuid::Uuid;

// ── internal wire format ─────────────────────────────────────────────────────

/// Wire format written to / read from the sea-streamer stream.
///
/// Carries the origin `instance_id` so the consumer pump can skip
/// messages produced by the same hub instance, avoiding double-delivery
/// to local subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaggedEnvelope {
    /// Random UUID assigned to the producing hub instance.
    instance_id: Uuid,
    /// The actual broadcast envelope.
    #[serde(flatten)]
    envelope: BroadcastEnvelope,
}

// ── SeaStreamerBroadcastHub ───────────────────────────────────────────────────

/// BroadcastHub implementation that fans out both locally and across
/// processes via sea-streamer.
///
/// Local subscribers (this process's WS handlers) are served by the inner
/// `InMemoryBroadcastHub` immediately on every `publish`. The same
/// serialised envelope is also written to a sea-streamer stream; a spawned
/// consumer pump drives any other process's hub — or, in loopback / test
/// mode, this process's own hub — by calling `local.publish` for each
/// inbound message whose `instance_id` differs from the hub's own ID.
pub struct SeaStreamerBroadcastHub {
    local: Arc<InMemoryBroadcastHub>,
    producer: sea_streamer::stdio::StdioProducer,
    instance_id: Uuid,
    /// Background task driving the consumer pump.
    /// Aborted on drop to avoid leaking across hub instances (important
    /// in tests that create multiple hubs).
    _consumer_task: JoinHandle<()>,
}

impl Drop for SeaStreamerBroadcastHub {
    fn drop(&mut self) {
        self._consumer_task.abort();
    }
}

impl SeaStreamerBroadcastHub {
    /// Connect using the stdio backend in normal (non-loopback) mode.
    ///
    /// `streamer_uri` — the streamer URI, e.g. `"stdio://"`.
    /// `stream_key`   — the stream name shared by all processes, e.g.
    ///                  `"suprnova-broadcast"`.
    ///
    /// # Errors
    ///
    /// Returns `FrameworkError::Internal` if the URI is invalid or the
    /// backend fails to connect.
    pub async fn new(streamer_uri: &str, stream_key: &str) -> Result<Self, FrameworkError> {
        Self::connect(streamer_uri, stream_key, false).await
    }

    /// Connect with stdio loopback enabled.
    ///
    /// With loopback, messages produced are fed back to consumers in the
    /// same process. **Use only in tests.** The duplicate-delivery guard
    /// (instance_id) ensures local subscribers still receive each event
    /// exactly once.
    pub async fn new_loopback(streamer_uri: &str, stream_key: &str) -> Result<Self, FrameworkError> {
        Self::connect(streamer_uri, stream_key, true).await
    }

    /// Internal constructor.
    async fn connect(
        streamer_uri: &str,
        stream_key_str: &str,
        loopback: bool,
    ) -> Result<Self, FrameworkError> {
        let uri = StreamerUri::from_str(streamer_uri).map_err(|e| {
            FrameworkError::internal(format!(
                "SeaStreamerBroadcastHub: invalid streamer URI \"{streamer_uri}\": {e}"
            ))
        })?;

        let stream_key = StreamKey::new(stream_key_str).map_err(|e| {
            FrameworkError::internal(format!(
                "SeaStreamerBroadcastHub: invalid stream key \"{stream_key_str}\": {e:?}"
            ))
        })?;

        let mut connect_opts = StdioConnectOptions::default();
        connect_opts.set_loopback(loopback);

        let streamer = StdioStreamer::connect(uri, connect_opts)
            .await
            .map_err(|e| {
                FrameworkError::internal(format!(
                    "SeaStreamerBroadcastHub: connect failed: {e}"
                ))
            })?;

        let producer = streamer
            .create_producer(stream_key.clone(), StdioProducerOptions::default())
            .await
            .map_err(|e| {
                FrameworkError::internal(format!(
                    "SeaStreamerBroadcastHub: create_producer failed: {e}"
                ))
            })?;

        let consumer = streamer
            .create_consumer(
                std::slice::from_ref(&stream_key),
                StdioConsumerOptions::new(ConsumerMode::RealTime),
            )
            .await
            .map_err(|e| {
                FrameworkError::internal(format!(
                    "SeaStreamerBroadcastHub: create_consumer failed: {e}"
                ))
            })?;

        let instance_id = Uuid::new_v4();
        let local = Arc::new(InMemoryBroadcastHub::new());

        let pump_local = Arc::clone(&local);
        let pump_instance_id = instance_id;
        let consumer_task = tokio::spawn(async move {
            consumer_pump_task(consumer, pump_local, pump_instance_id).await;
        });

        Ok(Self {
            local,
            producer,
            instance_id,
            _consumer_task: consumer_task,
        })
    }
}

/// Long-running task that reads from the sea-streamer consumer and pumps
/// decoded envelopes into the local in-memory hub.
///
/// Envelopes whose `instance_id` matches `own_id` are skipped — they were
/// produced by this hub instance and already delivered directly to local
/// subscribers in `publish()`. Without this guard, loopback mode (and any
/// round-trip through the broker) would cause double delivery.
async fn consumer_pump_task(
    consumer: sea_streamer::stdio::StdioConsumer,
    local: Arc<InMemoryBroadcastHub>,
    own_id: Uuid,
) {
    loop {
        match consumer.next().await {
            Ok(msg) => {
                let payload = msg.message();
                let bytes = payload.as_bytes();
                match serde_json::from_slice::<TaggedEnvelope>(bytes) {
                    Ok(tagged) if tagged.instance_id == own_id => {
                        // Our own message reflected back — skip to avoid
                        // double delivery to local subscribers.
                    }
                    Ok(tagged) => {
                        local.publish(tagged.envelope).await;
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "sea-streamer consumer: payload is not a valid TaggedEnvelope; dropping"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "sea-streamer consumer: receive error; backing off 1s"
                );
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

// ── BroadcastHub impl ────────────────────────────────────────────────────────

#[async_trait]
impl BroadcastHub for SeaStreamerBroadcastHub {
    fn subscribe(&self, channel: &str) -> tokio::sync::broadcast::Receiver<BroadcastEnvelope> {
        self.local.subscribe(channel)
    }

    async fn publish(&self, envelope: BroadcastEnvelope) {
        // Local fanout — immediate, no round-trip.
        self.local.publish(envelope.clone()).await;

        // Cross-process fanout via sea-streamer.
        let tagged = TaggedEnvelope {
            instance_id: self.instance_id,
            envelope,
        };
        match serde_json::to_vec(&tagged) {
            Ok(bytes) => {
                // send() is non-blocking; the future carries the delivery receipt
                // but we don't need it. Errors (e.g. producer shut down) are logged.
                match self.producer.send(bytes.as_slice()) {
                    Ok(_future) => {
                        // Receipt future intentionally not awaited; fire-and-forget.
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "SeaStreamerBroadcastHub: producer send error"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "SeaStreamerBroadcastHub: failed to serialize envelope"
                );
            }
        }
    }

    fn subscriber_count(&self, channel: &str) -> usize {
        self.local.subscriber_count(channel)
    }

    async fn track_member(&self, channel: &str, member_id: &str, info: Value) {
        // v1: local only. Cross-process presence is deferred to 7B+.
        self.local.track_member(channel, member_id, info).await;
    }

    async fn untrack_member(&self, channel: &str, member_id: &str) {
        self.local.untrack_member(channel, member_id).await;
    }

    async fn list_members(&self, channel: &str) -> Vec<Value> {
        self.local.list_members(channel).await
    }
}
