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
//!                ├─ channel == __presence__ ─► apply_presence_event (cross_process_view)
//!                │
//!                └─ other channels (skip own instance_id) ──► InMemoryBroadcastHub::publish
//! ```
//!
//! ## Duplicate-delivery guard
//!
//! Each hub instance is assigned a random `Uuid` on construction. Envelopes
//! are wrapped in a `TaggedEnvelope` that carries the origin `instance_id`.
//! The consumer pump drops non-presence messages whose `instance_id` matches
//! the local hub's own ID — this prevents local subscribers from seeing each
//! event twice (once from the direct local publish and once looped back through
//! the stream).
//!
//! Presence meta-channel messages are **not** skipped based on instance_id —
//! each hub needs its own events to appear in the cross_process_view so the
//! read path is unified (all members, local and remote, flow through the same
//! replicated view).
//!
//! ## Cross-process presence
//!
//! `track_member` / `untrack_member` update both a local `cross_process_view`
//! cache (for immediate write-after-read consistency) and publish a
//! `PresenceEvent` to the `__presence__` meta-channel so other process
//! instances can update their own views.
//!
//! `list_members` reads exclusively from `cross_process_view`, which includes
//! all members regardless of origin. Members whose process died without
//! sending `MemberRemoved` are pruned by a periodic pruning task once their
//! `last_seen` exceeds `PRESENCE_TTL`. A heartbeat task re-publishes local
//! members every `HEARTBEAT_INTERVAL` to refresh `last_seen` on all consumers.
//!
//! ## Backends
//!
//! The implementation uses `sea_streamer::stdio::StdioStreamer` (stdin/stdout
//! pipes). For production, swap in `sea_streamer_kafka::KafkaStreamer` or
//! `sea_streamer_redis::RedisStreamer` — they all implement the same
//! `sea_streamer_types::Streamer` trait. The URI scheme drives the backend
//! when using the socket adapter (`sea-streamer` with the `socket` feature).
//!
//! ## Loopback mode
//!
//! `SeaStreamerBroadcastHub::new_loopback` enables the stdio loopback option,
//! which feeds produced messages back to consumers in the same process. This
//! is intended for testing only — the duplicate guard (instance_id) ensures
//! the local hub still sees each app-data event only once.

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
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock as AsyncRwLock;
use tokio::task::JoinHandle;
use uuid::Uuid;

// ── constants ─────────────────────────────────────────────────────────────────

/// Reserved meta-channel name for presence replication. The consumer pump
/// routes envelopes on this channel to the presence view rather than the
/// local broadcast hub. App code should never publish to this channel name.
const PRESENCE_META_CHANNEL: &str = "__presence__";

/// How often each hub re-publishes its local members as heartbeats. Must be
/// well below `PRESENCE_TTL` so remote hubs see a continuous liveness signal.
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// How often the pruning task scans for stale entries.
const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Entries whose `last_seen` is older than this are dropped. Handles processes
/// that crashed without publishing `MemberRemoved`.
const PRESENCE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

// ── internal wire format ─────────────────────────────────────────────────────

/// Wire format written to / read from the sea-streamer stream.
///
/// Carries the origin `instance_id` so the consumer pump can skip
/// non-presence messages produced by the same hub instance, avoiding
/// double-delivery to local subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaggedEnvelope {
    /// Random UUID assigned to the producing hub instance.
    instance_id: Uuid,
    /// The actual broadcast envelope.
    #[serde(flatten)]
    envelope: BroadcastEnvelope,
}

// ── presence wire types ───────────────────────────────────────────────────────

/// Presence events published to the `__presence__` meta-channel.
///
/// Consumers deserialize `BroadcastEnvelope.data` from this channel as a
/// `PresenceEvent` and update their `cross_process_view` accordingly.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PresenceEvent {
    MemberAdded {
        instance_id: String,
        channel: String,
        member_id: String,
        info: Value,
        timestamp_ms: u64,
    },
    MemberRemoved {
        instance_id: String,
        channel: String,
        member_id: String,
        timestamp_ms: u64,
    },
    /// Re-published by the heartbeat task so remote consumers refresh
    /// `last_seen`. Treated identically to `MemberAdded` (upsert).
    Heartbeat {
        instance_id: String,
        channel: String,
        member_id: String,
        info: Value,
        timestamp_ms: u64,
    },
}

/// A tracked presence member in the replicated view.
#[derive(Clone, Debug)]
struct MemberRecord {
    info: Value,
    /// Updated every time we receive any presence event (add, heartbeat)
    /// for this (instance_id, member_id) pair. Stale entries are pruned.
    last_seen: Instant,
}

/// Returns milliseconds since UNIX_EPOCH — used as a lightweight logical
/// timestamp in presence events.
///
/// Uses `u64` rather than `u128` because `serde_json` does not support
/// `u128` by default. Unix milliseconds fit comfortably in `u64` for
/// the next ~550 million years.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── cross-process view type alias ─────────────────────────────────────────────

/// Outer key: channel name.
/// Inner key: (instance_id_string, member_id) — uniquely identifies a member
/// across all hub instances.
type CrossProcessView = Arc<AsyncRwLock<HashMap<String, HashMap<(String, String), MemberRecord>>>>;

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
///
/// Presence state is replicated across processes via the `__presence__`
/// meta-channel. See module-level docs for the full design.
pub struct SeaStreamerBroadcastHub {
    local: Arc<InMemoryBroadcastHub>,
    producer: sea_streamer::stdio::StdioProducer,
    instance_id: Uuid,
    /// Replicated presence view: merged local + remote members.
    cross_process_view: CrossProcessView,
    /// Snapshot of locally-tracked members for the heartbeat task. Updated
    /// by `track_member` / `untrack_member`.
    local_members: Arc<AsyncRwLock<HashMap<(String, String), Value>>>,
    /// Background task driving the consumer/presence pump.
    _consumer_task: JoinHandle<()>,
    /// Periodic heartbeat re-publisher.
    _heartbeat_task: JoinHandle<()>,
    /// Periodic stale-entry pruner.
    _prune_task: JoinHandle<()>,
}

impl Drop for SeaStreamerBroadcastHub {
    fn drop(&mut self) {
        self._consumer_task.abort();
        self._heartbeat_task.abort();
        self._prune_task.abort();
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
    /// (instance_id) ensures local subscribers still receive each app-data
    /// event exactly once. Presence events round-trip intentionally.
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
        let cross_process_view: CrossProcessView =
            Arc::new(AsyncRwLock::new(HashMap::new()));
        let local_members: Arc<AsyncRwLock<HashMap<(String, String), Value>>> =
            Arc::new(AsyncRwLock::new(HashMap::new()));

        // Spawn the unified consumer pump — handles both app-data envelopes
        // (routed to local hub) and presence meta-channel envelopes (routed
        // to cross_process_view).
        let pump_local = Arc::clone(&local);
        let pump_instance_id = instance_id;
        let pump_view = Arc::clone(&cross_process_view);
        let consumer_task = tokio::spawn(async move {
            consumer_pump_task(consumer, pump_local, pump_instance_id, pump_view).await;
        });

        // Spawn the heartbeat task — re-publishes local members every
        // HEARTBEAT_INTERVAL so other process instances refresh last_seen.
        let hb_producer = producer.clone();
        let hb_instance_id = instance_id.to_string();
        let hb_local_members = Arc::clone(&local_members);
        let heartbeat_task = tokio::spawn(async move {
            heartbeat_task(hb_producer, hb_instance_id, hb_local_members).await;
        });

        // Spawn the pruning task — drops MemberRecord entries whose last_seen
        // exceeds PRESENCE_TTL, cleaning up after crashed processes.
        let prune_view = Arc::clone(&cross_process_view);
        let prune_task = tokio::spawn(async move {
            prune_task(prune_view).await;
        });

        Ok(Self {
            local,
            producer,
            instance_id,
            cross_process_view,
            local_members,
            _consumer_task: consumer_task,
            _heartbeat_task: heartbeat_task,
            _prune_task: prune_task,
        })
    }

    /// Serialize a `PresenceEvent` and send it to the stream via the producer.
    ///
    /// The event is wrapped in a `TaggedEnvelope` on the `__presence__`
    /// meta-channel. The consumer pump routes it to `apply_presence_event`
    /// on all receiving instances (including this one in loopback / same-stream
    /// scenarios).
    fn send_presence_event(&self, event: &PresenceEvent) {
        let data = match serde_json::to_value(event) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "SeaStreamerBroadcastHub: failed to serialize PresenceEvent"
                );
                return;
            }
        };
        let event_name = match event {
            PresenceEvent::MemberAdded { .. } => "member_added",
            PresenceEvent::MemberRemoved { .. } => "member_removed",
            PresenceEvent::Heartbeat { .. } => "heartbeat",
        };
        let tagged = TaggedEnvelope {
            instance_id: self.instance_id,
            envelope: BroadcastEnvelope {
                channel: PRESENCE_META_CHANNEL.to_string(),
                event: event_name.to_string(),
                data,
            },
        };
        match serde_json::to_vec(&tagged) {
            Ok(bytes) => match self.producer.send(bytes.as_slice()) {
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "SeaStreamerBroadcastHub: presence producer send error"
                    );
                }
            },
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "SeaStreamerBroadcastHub: failed to serialize presence TaggedEnvelope"
                );
            }
        }
    }
}

// ── background tasks ──────────────────────────────────────────────────────────

/// Long-running task that reads from the sea-streamer consumer and routes:
///
/// - `__presence__` channel envelopes → `apply_presence_event` (updates
///   the cross_process_view for all processes including this one).
/// - All other channels → local hub (skipping own instance_id to prevent
///   double-delivery for app-data envelopes).
async fn consumer_pump_task(
    consumer: sea_streamer::stdio::StdioConsumer,
    local: Arc<InMemoryBroadcastHub>,
    own_id: Uuid,
    cross_view: CrossProcessView,
) {
    loop {
        match consumer.next().await {
            Ok(msg) => {
                let payload = msg.message();
                let bytes = payload.as_bytes();
                match serde_json::from_slice::<TaggedEnvelope>(bytes) {
                    Ok(tagged) if tagged.envelope.channel == PRESENCE_META_CHANNEL => {
                        // Presence meta-channel — update the replicated view.
                        // We process our OWN presence events too (no instance_id skip)
                        // so our members appear in cross_process_view via the same
                        // code path as remote members.
                        match serde_json::from_value::<PresenceEvent>(tagged.envelope.data) {
                            Ok(pe) => apply_presence_event(&cross_view, pe).await,
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "sea-streamer consumer: __presence__ payload is not a valid PresenceEvent; dropping"
                                );
                            }
                        }
                    }
                    Ok(tagged) if tagged.instance_id == own_id => {
                        // Our own app-data message reflected back — skip to avoid
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

/// Apply a `PresenceEvent` to the cross-process view. Called by the consumer
/// pump for every inbound `__presence__` message.
async fn apply_presence_event(view: &CrossProcessView, event: PresenceEvent) {
    let mut map = view.write().await;
    match event {
        PresenceEvent::MemberAdded {
            instance_id,
            channel,
            member_id,
            info,
            ..
        }
        | PresenceEvent::Heartbeat {
            instance_id,
            channel,
            member_id,
            info,
            ..
        } => {
            map.entry(channel)
                .or_default()
                .insert((instance_id, member_id), MemberRecord {
                    info,
                    last_seen: Instant::now(),
                });
        }
        PresenceEvent::MemberRemoved {
            instance_id,
            channel,
            member_id,
            ..
        } => {
            if let Some(ch_map) = map.get_mut(&channel) {
                ch_map.remove(&(instance_id, member_id));
            }
        }
    }
}

/// Periodically re-publishes all locally-tracked members as `Heartbeat`
/// events. This refreshes `last_seen` on all consumers — including remote
/// process instances that started after the original `MemberAdded` was
/// published — so stale TTL pruning doesn't evict live members.
async fn heartbeat_task(
    producer: sea_streamer::stdio::StdioProducer,
    instance_id: String,
    local_members: Arc<AsyncRwLock<HashMap<(String, String), Value>>>,
) {
    loop {
        tokio::time::sleep(HEARTBEAT_INTERVAL).await;
        let snapshot = {
            let guard = local_members.read().await;
            guard.clone()
        };
        for ((channel, member_id), info) in snapshot {
            let event = PresenceEvent::Heartbeat {
                instance_id: instance_id.clone(),
                channel,
                member_id,
                info,
                timestamp_ms: now_ms(),
            };
            send_presence_via_producer(&producer, &event, &instance_id);
        }
    }
}

/// Periodically drops cross_process_view entries whose `last_seen` is older
/// than `PRESENCE_TTL`. Cleans up after processes that crashed without
/// publishing `MemberRemoved`.
async fn prune_task(view: CrossProcessView) {
    loop {
        tokio::time::sleep(PRUNE_INTERVAL).await;
        let now = Instant::now();
        let mut map = view.write().await;
        for ch_map in map.values_mut() {
            ch_map.retain(|_, record| {
                now.duration_since(record.last_seen) < PRESENCE_TTL
            });
        }
        // Also remove empty channel entries.
        map.retain(|_, ch_map| !ch_map.is_empty());
    }
}

/// Helper used by the heartbeat task to serialize and send a PresenceEvent.
/// Mirrors `SeaStreamerBroadcastHub::send_presence_event` but is a free
/// function so it can be called from the spawned task without capturing
/// `self`.
fn send_presence_via_producer(
    producer: &sea_streamer::stdio::StdioProducer,
    event: &PresenceEvent,
    instance_id_str: &str,
) {
    let data = match serde_json::to_value(event) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "heartbeat task: failed to serialize PresenceEvent");
            return;
        }
    };
    let event_name = match event {
        PresenceEvent::MemberAdded { .. } => "member_added",
        PresenceEvent::MemberRemoved { .. } => "member_removed",
        PresenceEvent::Heartbeat { .. } => "heartbeat",
    };
    // Build a dummy Uuid from the string for the TaggedEnvelope.
    let uuid = Uuid::parse_str(instance_id_str).unwrap_or_else(|_| Uuid::nil());
    let tagged = TaggedEnvelope {
        instance_id: uuid,
        envelope: BroadcastEnvelope {
            channel: PRESENCE_META_CHANNEL.to_string(),
            event: event_name.to_string(),
            data,
        },
    };
    match serde_json::to_vec(&tagged) {
        Ok(bytes) => {
            if let Err(e) = producer.send(bytes.as_slice()) {
                tracing::error!(error = %e, "heartbeat task: producer send error");
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "heartbeat task: failed to serialize TaggedEnvelope");
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
        // 1. Update the local replicated view immediately for write-after-read
        //    consistency — callers don't need to wait for the round-trip.
        {
            let mut map = self.cross_process_view.write().await;
            map.entry(channel.to_string())
                .or_default()
                .insert(
                    (self.instance_id.to_string(), member_id.to_string()),
                    MemberRecord {
                        info: info.clone(),
                        last_seen: Instant::now(),
                    },
                );
        }

        // 2. Keep the heartbeat snapshot in sync.
        {
            let mut local_members = self.local_members.write().await;
            local_members.insert(
                (channel.to_string(), member_id.to_string()),
                info.clone(),
            );
        }

        // 3. Also update the local hub so existing in-process APIs still work.
        self.local.track_member(channel, member_id, info.clone()).await;

        // 4. Publish to the meta-channel so other processes update their views.
        //    In loopback/single-stream tests this round-trips back to us, but
        //    `apply_presence_event` is idempotent (upsert), so duplicates are harmless.
        self.send_presence_event(&PresenceEvent::MemberAdded {
            instance_id: self.instance_id.to_string(),
            channel: channel.to_string(),
            member_id: member_id.to_string(),
            info,
            timestamp_ms: now_ms(),
        });
    }

    async fn untrack_member(&self, channel: &str, member_id: &str) {
        // 1. Remove from the replicated view immediately.
        {
            let mut map = self.cross_process_view.write().await;
            if let Some(ch_map) = map.get_mut(channel) {
                ch_map.remove(&(self.instance_id.to_string(), member_id.to_string()));
            }
        }

        // 2. Remove from heartbeat snapshot.
        {
            let mut local_members = self.local_members.write().await;
            local_members.remove(&(channel.to_string(), member_id.to_string()));
        }

        // 3. Remove from local hub.
        self.local.untrack_member(channel, member_id).await;

        // 4. Publish removal to the meta-channel for other processes.
        self.send_presence_event(&PresenceEvent::MemberRemoved {
            instance_id: self.instance_id.to_string(),
            channel: channel.to_string(),
            member_id: member_id.to_string(),
            timestamp_ms: now_ms(),
        });
    }

    async fn list_members(&self, channel: &str) -> Vec<Value> {
        // Read from the unified cross_process_view — includes local members
        // (written directly in track_member) and remote members (written by
        // the consumer pump from inbound presence events).
        let map = self.cross_process_view.read().await;
        map.get(channel)
            .map(|ch_map| ch_map.values().map(|r| r.info.clone()).collect())
            .unwrap_or_default()
    }
}
