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
//!       └─► SeaProducer::send (serialized JSON to sea-streamer stream)
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
//! `last_seen` exceeds the configured TTL (default 60 s). A heartbeat task
//! re-publishes local members at `ttl / 6` so remote hubs see a continuous
//! liveness signal and don't prune live members.
//!
//! ## Backends
//!
//! The hub uses sea-streamer's **socket adapter** (`SeaStreamer`,
//! `SeaProducer`, `SeaConsumer`), which is an enum-dispatched wrapper over
//! every backend compiled into the `sea-streamer` dependency. The backend is
//! selected at runtime from the URI scheme:
//!
//! | URI scheme           | Backend                                  | Production-ready |
//! |----------------------|------------------------------------------|------------------|
//! | `stdio://`           | stdin/stdout pipes (tests, single-proc)  | No               |
//! | `redis://` `rediss://` | Redis Streams (`sea-streamer-redis`)  | **Yes**          |
//! | `kafka://` `kafka+ssl://` | Kafka (if `sea-streamer-kafka` is enabled) | **Yes** |
//! | `file://`            | Local file (`sea-streamer-file`)         | No               |
//!
//! The default Suprnova build enables `stdio` + `redis` + `socket`. To enable
//! Kafka, add `kafka` to the `sea-streamer` feature set in `framework/Cargo.toml`
//! (it pulls in `sea-streamer-kafka`).
//!
//! For multi-process deployments, use `redis://host:6379` (or `rediss://` for
//! TLS). Redis Streams persists events, supports consumer groups, and survives
//! a hub restart — the cross-process fanout works exactly like the loopback
//! test scenario.
//!
//! ## Loopback mode
//!
//! `SeaStreamerBroadcastHub::new_loopback` enables the stdio loopback option,
//! which feeds produced messages back to consumers in the same process. This
//! is intended for **testing only** — the duplicate guard (instance_id) ensures
//! the local hub still sees each app-data event only once. Loopback is a
//! stdio-specific option; if you pass `loopback = true` with a non-stdio URI
//! the option is silently ignored by the non-stdio backends (each one has its
//! own native cross-process behaviour).

use crate::FrameworkError;
use crate::broadcasting::hub::{
    BroadcastEnvelope, BroadcastHub, InMemoryBroadcastHub, reject_reserved_channel,
};
use async_trait::async_trait;
use futures::FutureExt;
use sea_streamer::{
    Buffer, Consumer, ConsumerMode, ConsumerOptions, Message, Producer, SeaConnectOptions,
    SeaConsumer, SeaConsumerOptions, SeaProducer, SeaProducerOptions, SeaStreamer, StreamKey,
    Streamer, StreamerUri,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
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

/// Default presence TTL. Entries whose `last_seen` is older than this are
/// dropped, handling processes that crashed without publishing `MemberRemoved`.
///
/// The heartbeat interval is derived as `TTL / 6` (10 s) and the prune scan
/// interval as `TTL / 2` (30 s). Use
/// [`SeaStreamerBroadcastHub::new_with_presence_ttl`] to override for tests.
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
    producer: SeaProducer,
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
    /// Uses the default presence TTL (60 s). See
    /// [`new_with_presence_ttl`](Self::new_with_presence_ttl) to override.
    ///
    /// # Errors
    ///
    /// Returns `FrameworkError::Internal` if the URI is invalid or the
    /// backend fails to connect.
    pub async fn new(streamer_uri: &str, stream_key: &str) -> Result<Self, FrameworkError> {
        Self::connect(streamer_uri, stream_key, false, PRESENCE_TTL).await
    }

    /// Connect with a custom presence TTL.
    ///
    /// `streamer_uri` — the streamer URI, e.g. `"stdio://"`.
    /// `stream_key`   — the stream name shared by all processes, e.g.
    ///                  `"suprnova-broadcast"`.
    ///
    /// Presence members whose `last_seen` exceeds `ttl` are pruned. The
    /// heartbeat interval is derived as `ttl / 6` so that live members
    /// are refreshed well within the TTL window.
    ///
    /// **Mostly useful for tests** — setting `ttl` to e.g.
    /// `Duration::from_millis(600)` gives a 100 ms heartbeat and a
    /// sub-second prune cycle, allowing the crash-recovery path to be
    /// exercised without multi-minute waits.
    ///
    /// Production deployments should leave the default TTL (60 s) unless
    /// they have a specific reason to shorten it (e.g., extremely volatile
    /// clusters where 60 s of stale presence is unacceptable).
    pub async fn new_with_presence_ttl(
        streamer_uri: &str,
        stream_key: &str,
        ttl: std::time::Duration,
    ) -> Result<Self, FrameworkError> {
        Self::connect(streamer_uri, stream_key, false, ttl).await
    }

    /// Connect with stdio loopback enabled.
    ///
    /// With loopback, messages produced are fed back to consumers in the
    /// same process. **Use only in tests.** The duplicate-delivery guard
    /// (instance_id) ensures local subscribers still receive each app-data
    /// event exactly once. Presence events round-trip intentionally.
    ///
    /// Uses the default presence TTL (60 s).
    pub async fn new_loopback(
        streamer_uri: &str,
        stream_key: &str,
    ) -> Result<Self, FrameworkError> {
        Self::connect(streamer_uri, stream_key, true, PRESENCE_TTL).await
    }

    /// Connect with loopback enabled and a custom presence TTL.
    ///
    /// Combines the loopback test mode with a configurable TTL for tests
    /// that need to exercise the crash-recovery / TTL-prune path quickly.
    pub async fn new_loopback_with_presence_ttl(
        streamer_uri: &str,
        stream_key: &str,
        ttl: std::time::Duration,
    ) -> Result<Self, FrameworkError> {
        Self::connect(streamer_uri, stream_key, true, ttl).await
    }

    /// Internal constructor.
    ///
    /// Backend selection is driven by the URI scheme — see the module-level
    /// "Backends" table. `loopback` is a stdio-only option (other backends
    /// have native cross-process behaviour); we set it on the stdio sub-options
    /// unconditionally and let other backends ignore it.
    async fn connect(
        streamer_uri: &str,
        stream_key_str: &str,
        loopback: bool,
        presence_ttl: std::time::Duration,
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

        let mut connect_opts = SeaConnectOptions::default();
        connect_opts.set_stdio_connect_options(|opts| {
            opts.set_loopback(loopback);
        });

        let streamer = SeaStreamer::connect(uri, connect_opts).await.map_err(|e| {
            FrameworkError::internal(format!("SeaStreamerBroadcastHub: connect failed: {e}"))
        })?;

        let producer = streamer
            .create_producer(stream_key.clone(), SeaProducerOptions::default())
            .await
            .map_err(|e| {
                FrameworkError::internal(format!(
                    "SeaStreamerBroadcastHub: create_producer failed: {e}"
                ))
            })?;

        let consumer = streamer
            .create_consumer(
                std::slice::from_ref(&stream_key),
                SeaConsumerOptions::new(ConsumerMode::RealTime),
            )
            .await
            .map_err(|e| {
                FrameworkError::internal(format!(
                    "SeaStreamerBroadcastHub: create_consumer failed: {e}"
                ))
            })?;

        // Derive heartbeat and prune intervals from the TTL:
        //   heartbeat = ttl / 6   (refresh well within the TTL window)
        //   prune     = ttl / 2   (scan twice per TTL window)
        //
        // Both are clamped to a non-zero floor so a pathologically small TTL
        // (e.g. sub-second values from a misconfigured caller) cannot produce
        // a `Duration::ZERO` interval, which would make the heartbeat / prune
        // loops sleep zero and busy-spin a worker thread.
        const MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
        let heartbeat_interval = (presence_ttl / 6).max(MIN_INTERVAL);
        let prune_interval = (presence_ttl / 2).max(MIN_INTERVAL);

        let instance_id = Uuid::new_v4();
        let local = Arc::new(InMemoryBroadcastHub::new());
        let cross_process_view: CrossProcessView = Arc::new(AsyncRwLock::new(HashMap::new()));
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
        // `heartbeat_interval` so other process instances refresh last_seen.
        let hb_producer = producer.clone();
        let hb_instance_id = instance_id.to_string();
        let hb_local_members = Arc::clone(&local_members);
        let heartbeat_task = tokio::spawn(async move {
            heartbeat_task(
                hb_producer,
                hb_instance_id,
                hb_local_members,
                heartbeat_interval,
            )
            .await;
        });

        // Spawn the pruning task — drops MemberRecord entries whose last_seen
        // exceeds `presence_ttl`, cleaning up after crashed processes.
        let prune_view = Arc::clone(&cross_process_view);
        let prune_task = tokio::spawn(async move {
            prune_task(prune_view, prune_interval, presence_ttl).await;
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
    ///
    /// Errors are logged with the channel + member_id context AND
    /// returned to the caller so the `track_member` /
    /// `untrack_member` trait surface can surface a producer-down
    /// state to the WS handler. The handler can then refuse the
    /// presence join cleanly rather than leaving peer views divergent
    /// for the entire heartbeat-reconciliation window.
    fn send_presence_event(&self, event: &PresenceEvent) -> Result<(), FrameworkError> {
        let (event_name, channel, member_id) = match event {
            PresenceEvent::MemberAdded {
                channel, member_id, ..
            } => ("member_added", channel.as_str(), member_id.as_str()),
            PresenceEvent::MemberRemoved {
                channel, member_id, ..
            } => ("member_removed", channel.as_str(), member_id.as_str()),
            PresenceEvent::Heartbeat {
                channel, member_id, ..
            } => ("heartbeat", channel.as_str(), member_id.as_str()),
        };
        let data = serde_json::to_value(event).map_err(|e| {
            tracing::error!(
                error = %e,
                event = event_name,
                channel,
                member_id,
                "SeaStreamerBroadcastHub: failed to serialize PresenceEvent"
            );
            FrameworkError::internal(format!(
                "SeaStreamerBroadcastHub: failed to serialize PresenceEvent ({event_name} on {channel}/{member_id}): {e}"
            ))
        })?;
        let tagged = TaggedEnvelope {
            instance_id: self.instance_id,
            envelope: BroadcastEnvelope::new(
                PRESENCE_META_CHANNEL.to_string(),
                event_name.to_string(),
                data,
            ),
        };
        let bytes = serde_json::to_vec(&tagged).map_err(|e| {
            tracing::error!(
                error = %e,
                event = event_name,
                channel,
                member_id,
                "SeaStreamerBroadcastHub: failed to serialize presence TaggedEnvelope"
            );
            FrameworkError::internal(format!(
                "SeaStreamerBroadcastHub: failed to serialize presence TaggedEnvelope: {e}"
            ))
        })?;
        self.producer.send(bytes.as_slice()).map_err(|e| {
            tracing::error!(
                error = %e,
                event = event_name,
                channel,
                member_id,
                "SeaStreamerBroadcastHub: presence producer send error"
            );
            FrameworkError::internal(format!(
                "SeaStreamerBroadcastHub: presence producer send error ({event_name} on {channel}/{member_id}): {e}"
            ))
        })?;
        Ok(())
    }
}

// ── background tasks ──────────────────────────────────────────────────────────

/// Run a per-loop-iteration body under `catch_unwind`. On a panic the payload
/// is logged at error level (tagged with `task_name`) and the iteration is
/// reported as "panicked" via `Err(())`; the caller decides whether to back
/// off before continuing.
///
/// Per-iteration scope is intentional: the long-lived resources (consumer,
/// producer, cross-process view, local-member snapshot) are owned by the
/// outer `loop` frame that never unwinds — only the body of one iteration
/// crosses the catch boundary. `cross_process_view` is a
/// `tokio::sync::RwLock` (does NOT poison on panic), so a panic mid-write
/// drops the guard and the next iteration acquires cleanly.
async fn run_iteration_guarded<F>(task_name: &'static str, body: F) -> Result<(), ()>
where
    F: Future<Output = ()>,
{
    match AssertUnwindSafe(body).catch_unwind().await {
        Ok(()) => Ok(()),
        Err(panic_payload) => {
            let msg = panic_payload
                .downcast_ref::<&'static str>()
                .map(|s| (*s).to_string())
                .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            tracing::error!(
                task = task_name,
                panic = %msg,
                "SeaStreamerBroadcastHub background task iteration panicked; continuing"
            );
            Err(())
        }
    }
}

/// Long-running task that reads from the sea-streamer consumer and routes:
///
/// - `__presence__` channel envelopes → `apply_presence_event` (updates
///   the cross_process_view for all processes including this one).
/// - All other channels → local hub (skipping own instance_id to prevent
///   double-delivery for app-data envelopes).
async fn consumer_pump_task(
    consumer: SeaConsumer,
    local: Arc<InMemoryBroadcastHub>,
    own_id: Uuid,
    cross_view: CrossProcessView,
) {
    // Receive-error backoff state. Pre-fix this was a fixed 1s sleep
    // per error, which busy-spins log volume when the upstream is
    // persistently down (Redis/Kafka offline for minutes). The
    // exponential schedule caps at 30s so transient blips still
    // recover within seconds while sustained outages don't drown the
    // log pipeline.
    //
    // The body of `run_iteration_guarded` returns `()`, so we signal
    // "receive error this round" through this atomic instead of
    // restructuring the helper's signature.
    use std::sync::atomic::{AtomicBool, Ordering};
    let receive_error_flag = Arc::new(AtomicBool::new(false));
    let mut receive_error_streak: u32 = 0;
    loop {
        receive_error_flag.store(false, Ordering::Relaxed);
        let receive_error_flag_inner = Arc::clone(&receive_error_flag);
        let outcome = run_iteration_guarded("consumer_pump", async {
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
                            // In-memory publish is infallible; logging is just
                            // a belt-and-braces guard against a future trait
                            // impl that needs to surface a failure here.
                            if let Err(e) = local.publish(tagged.envelope).await {
                                tracing::warn!(
                                    error = %e,
                                    "sea-streamer consumer: local hub publish failed; dropping"
                                );
                            }
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
                        "sea-streamer consumer: receive error; backing off (escalating)"
                    );
                    receive_error_flag_inner.store(true, Ordering::Relaxed);
                }
            }
        })
        .await;

        // Receive errors escalate: 1s, 2s, 4s, 8s, 16s, 30s (capped).
        // Reset the streak as soon as a non-error iteration runs so
        // a single transient blip doesn't slow recovery.
        if receive_error_flag.load(Ordering::Relaxed) {
            receive_error_streak = receive_error_streak.saturating_add(1);
            let secs = (1u64 << receive_error_streak.saturating_sub(1).min(5)).min(30);
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        } else {
            receive_error_streak = 0;
        }

        // If the iteration panicked, back off briefly before the next attempt
        // so a hot-loop panic source (e.g. consumer.next() itself) cannot
        // busy-spin a worker thread.
        if outcome.is_err() {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
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
            map.entry(channel).or_default().insert(
                (instance_id, member_id),
                MemberRecord {
                    info,
                    last_seen: Instant::now(),
                },
            );
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
    producer: SeaProducer,
    instance_id: String,
    local_members: Arc<AsyncRwLock<HashMap<(String, String), Value>>>,
    interval: std::time::Duration,
) {
    loop {
        tokio::time::sleep(interval).await;
        // Body runs under catch_unwind; producer/local_members/instance_id are
        // borrowed by reference and stay owned by the outer loop frame, so a
        // panic here cannot kill the task or leak resources.
        let _ = run_iteration_guarded("heartbeat", async {
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
        })
        .await;
    }
}

/// Periodically drops cross_process_view entries whose `last_seen` is older
/// than `ttl`. Cleans up after processes that crashed without publishing
/// `MemberRemoved`. All entries are pruned uniformly regardless of which
/// hub instance produced them — including this hub's own instance_id, which
/// is important for crash-recovery tests where a dropped hub's heartbeat
/// task has been aborted.
async fn prune_task(
    view: CrossProcessView,
    interval: std::time::Duration,
    ttl: std::time::Duration,
) {
    loop {
        tokio::time::sleep(interval).await;
        // Body runs under catch_unwind; `view` is borrowed by reference and
        // stays owned by the outer loop frame. tokio::sync::RwLock does not
        // poison on panic, so a panic mid-write drops the guard cleanly and
        // the next iteration acquires the lock as normal.
        let _ = run_iteration_guarded("prune", async {
            let now = Instant::now();
            let mut map = view.write().await;
            for ch_map in map.values_mut() {
                ch_map.retain(|_, record| now.duration_since(record.last_seen) < ttl);
            }
            // Also remove empty channel entries.
            map.retain(|_, ch_map| !ch_map.is_empty());
        })
        .await;
    }
}

/// Helper used by the heartbeat task to serialize and send a PresenceEvent.
/// Mirrors `SeaStreamerBroadcastHub::send_presence_event` but is a free
/// function so it can be called from the spawned task without capturing
/// `self`.
fn send_presence_via_producer(
    producer: &SeaProducer,
    event: &PresenceEvent,
    instance_id_str: &str,
) {
    let (event_name, channel, member_id) = match event {
        PresenceEvent::MemberAdded {
            channel, member_id, ..
        } => ("member_added", channel.as_str(), member_id.as_str()),
        PresenceEvent::MemberRemoved {
            channel, member_id, ..
        } => ("member_removed", channel.as_str(), member_id.as_str()),
        PresenceEvent::Heartbeat {
            channel, member_id, ..
        } => ("heartbeat", channel.as_str(), member_id.as_str()),
    };
    let data = match serde_json::to_value(event) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                error = %e,
                event = event_name,
                channel,
                member_id,
                "heartbeat task: failed to serialize PresenceEvent"
            );
            return;
        }
    };
    // Build a dummy Uuid from the string for the TaggedEnvelope.
    let uuid = Uuid::parse_str(instance_id_str).unwrap_or_else(|_| Uuid::nil());
    let tagged = TaggedEnvelope {
        instance_id: uuid,
        envelope: BroadcastEnvelope::new(
            PRESENCE_META_CHANNEL.to_string(),
            event_name.to_string(),
            data,
        ),
    };
    match serde_json::to_vec(&tagged) {
        Ok(bytes) => {
            if let Err(e) = producer.send(bytes.as_slice()) {
                tracing::error!(
                    error = %e,
                    event = event_name,
                    channel,
                    member_id,
                    "heartbeat task: producer send error"
                );
            }
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                event = event_name,
                channel,
                member_id,
                "heartbeat task: failed to serialize TaggedEnvelope"
            );
        }
    }
}

// ── BroadcastHub impl ────────────────────────────────────────────────────────

#[async_trait]
impl BroadcastHub for SeaStreamerBroadcastHub {
    fn subscribe(&self, channel: &str) -> tokio::sync::broadcast::Receiver<BroadcastEnvelope> {
        self.local.subscribe(channel)
    }

    async fn publish(&self, envelope: BroadcastEnvelope) -> Result<(), FrameworkError> {
        // Reject reserved meta-channel names BEFORE any side effect. A
        // publish to `__presence__` here would serialise a TaggedEnvelope
        // to the stream that every peer's consumer pump routes straight
        // into `apply_presence_event` — injecting phantom presence into
        // every process's `cross_process_view`. The hub itself fans
        // presence out via a dedicated producer path (`send_presence_event`)
        // that never traverses this method, so the guard never blocks
        // legitimate framework traffic.
        reject_reserved_channel(&envelope.channel)?;

        // Local fanout — immediate, no round-trip. Local subscribers
        // see this envelope even if the cross-process fanout below
        // fails: the local delivery and the wire delivery are
        // independent and we don't want a broker hiccup to silently
        // drop in-process listeners.
        self.local.publish(envelope.clone()).await?;

        // Cross-process fanout via sea-streamer. A failure here is a
        // real loss — other processes' subscribers won't see the event.
        // Surface it to the caller so a Broadcastable dispatch returns
        // Err and the operator can react.
        let tagged = TaggedEnvelope {
            instance_id: self.instance_id,
            envelope,
        };
        let bytes = serde_json::to_vec(&tagged).map_err(|e| {
            FrameworkError::internal(format!(
                "SeaStreamerBroadcastHub: failed to serialize envelope: {e}"
            ))
        })?;
        // send() is non-blocking; the future carries the delivery receipt
        // but we don't need it (fire-and-forget at the broker boundary).
        // Producer-side failures (broker disconnected, channel closed) are
        // returned to the caller.
        self.producer.send(bytes.as_slice()).map_err(|e| {
            FrameworkError::internal(format!("SeaStreamerBroadcastHub: producer send error: {e}"))
        })?;
        Ok(())
    }

    fn subscriber_count(&self, channel: &str) -> usize {
        self.local.subscriber_count(channel)
    }

    async fn track_member(
        &self,
        channel: &str,
        member_id: &str,
        info: Value,
    ) -> Result<(), FrameworkError> {
        // 1. Update the local replicated view immediately for write-after-read
        //    consistency — callers don't need to wait for the round-trip.
        {
            let mut map = self.cross_process_view.write().await;
            map.entry(channel.to_string()).or_default().insert(
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
            local_members.insert((channel.to_string(), member_id.to_string()), info.clone());
        }

        // 3. Also update the local hub so existing in-process APIs still work.
        self.local
            .track_member(channel, member_id, info.clone())
            .await?;

        // 4. Publish to the meta-channel so other processes update their views.
        //    In loopback/single-stream tests this round-trips back to us, but
        //    `apply_presence_event` is idempotent (upsert), so duplicates are harmless.
        //    Producer failure surfaces to the caller so the WS handler can
        //    refuse the join cleanly instead of leaving peer views divergent.
        self.send_presence_event(&PresenceEvent::MemberAdded {
            instance_id: self.instance_id.to_string(),
            channel: channel.to_string(),
            member_id: member_id.to_string(),
            info,
            timestamp_ms: now_ms(),
        })
    }

    async fn untrack_member(&self, channel: &str, member_id: &str) -> Result<(), FrameworkError> {
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
        self.local.untrack_member(channel, member_id).await?;

        // 4. Publish removal to the meta-channel for other processes.
        self.send_presence_event(&PresenceEvent::MemberRemoved {
            instance_id: self.instance_id.to_string(),
            channel: channel.to_string(),
            member_id: member_id.to_string(),
            timestamp_ms: now_ms(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic inside the guarded iteration must NOT propagate to the caller.
    /// This is the resilience primitive the three background tasks
    /// (consumer_pump, heartbeat, prune) all depend on: if this swallows a
    /// panic and returns `Err(())`, those tasks' surrounding `loop { … }`
    /// frames continue to the next iteration instead of silently dying and
    /// freezing cross-process fanout, presence replication, or pruning.
    #[tokio::test]
    async fn guarded_iteration_swallows_str_panic() {
        let outcome = run_iteration_guarded("test", async { panic!("boom") }).await;
        assert!(outcome.is_err(), "panicking body should be reported as Err");
    }

    /// Same contract for a `String` panic payload (the other common shape).
    #[tokio::test]
    async fn guarded_iteration_swallows_string_panic() {
        let outcome = run_iteration_guarded("test", async {
            panic!("boom: {}", String::from("dynamic"));
        })
        .await;
        assert!(outcome.is_err());
    }

    /// A non-panicking body should return `Ok(())` — i.e. the guard only
    /// fires on real panics, not on every successful iteration.
    #[tokio::test]
    async fn guarded_iteration_passes_through_success() {
        let outcome = run_iteration_guarded("test", async { /* no-op */ }).await;
        assert!(outcome.is_ok());
    }

    /// Successive panicking iterations must each be caught independently —
    /// proving the helper is reentrant and not a one-shot guard.
    #[tokio::test]
    async fn guarded_iteration_handles_repeated_panics() {
        for _ in 0..3 {
            let outcome = run_iteration_guarded("test", async { panic!("repeat") }).await;
            assert!(outcome.is_err());
        }
        let outcome = run_iteration_guarded("test", async { /* finally normal */ }).await;
        assert!(outcome.is_ok());
    }
}
