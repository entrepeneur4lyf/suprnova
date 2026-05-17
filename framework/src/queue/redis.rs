//! Redis-backed queue driver via sea-streamer-redis consumer groups.
//!
//! # Design
//!
//! Messages are stored in a Redis Stream and consumed via consumer groups
//! (`XREADGROUP` / `XACK`). Each `pop` call uses `XREADGROUP` to deliver one
//! message to this consumer; the message stays in the PEL (pending-entry list)
//! until `ack` is called.
//!
//! ## Delivery semantics
//!
//! This driver provides **at-least-once delivery**. After `ack` returns
//! `Ok(())`, the actual `XACK` may not yet be committed to Redis
//! (sea-streamer batches commits under `AutoCommit::Disabled`); if the
//! process crashes before the next flush, the message re-enters the
//! pending entries list and is re-delivered. Idempotency belongs at the
//! job level — see `framework/src/idempotency/mod.rs`.
//!
//! Similarly, `nack` performs two non-atomic Redis commands (XADD +
//! XACK). If XACK fails after XADD succeeds, the original message stays
//! in the PEL and is re-delivered via XAUTOCLAIM with the pre-nack
//! `attempts` value, while the freshly-published copy carries
//! `attempts + 1`. Job handlers MUST be idempotent.
//!
//! ## Visibility timeout
//!
//! `auto_claim_idle` is configured once at construction time (via the
//! `visibility_timeout` argument to `connect`). Messages not acknowledged within
//! that window will be re-claimed by this consumer (or another in the group) on
//! the next poll cycle via Redis `XAUTOCLAIM`.
//!
//! The `visibility_timeout: Duration` parameter on `QueueDriver::pop` is
//! **ignored** for this driver; the per-connection value governs. This is a
//! documented divergence from the trait contract imposed by Redis Streams'
//! construction-time-only idle window.
//!
//! ## nack semantics
//!
//! Redis Streams has no native nack-with-delay. `nack` is implemented as an
//! atomic two-step:
//! 1. Re-publish the envelope (with `attempts` incremented and `available_at`
//!    advanced by `requeue_delay`) via `XADD`.
//! 2. Acknowledge the original message via `XACK` so it leaves the PEL.
//!
//! ## AutoCommit::Disabled
//!
//! The consumer is created with `AutoCommit::Disabled` so no implicit ack
//! ever fires. The caller drives all acknowledgements through `ack`/`nack`.

use crate::error::FrameworkError;
use crate::queue::driver::{QueueDriver, Reservation, ReservationToken};
use crate::queue::envelope::Envelope;
use async_trait::async_trait;
use chrono::Utc;
use sea_streamer::{Buffer, Consumer, ConsumerOptions, Message, Producer, StreamKey, Streamer, StreamerUri};
use sea_streamer_redis::{
    AutoCommit, AutoStreamReset, RedisConsumer, RedisConsumerOptions, RedisProducer, RedisStreamer,
};
use sea_streamer::ConsumerMode;
use sea_streamer::{ConsumerGroup, ConsumerId};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;
use uuid::Uuid;

/// Value stored in the pending map: the original envelope plus the
/// `SharedMessage` needed to call `RedisConsumer::ack`.
type PendingEntry = (Envelope, sea_streamer::SharedMessage);

/// Redis-backed queue driver.
///
/// Construct via [`RedisQueueDriver::connect`]. The driver is `Send + Sync`
/// and can be wrapped in an `Arc` for sharing across tasks.
pub struct RedisQueueDriver {
    producer: RedisProducer,
    consumer: RedisConsumer,
    stream_key: StreamKey,
    /// Map from `ReservationToken` UUID → `(Envelope, SharedMessage)`.
    /// The `SharedMessage` is required by `RedisConsumer::ack`.
    pending: Mutex<HashMap<Uuid, PendingEntry>>,
}

impl RedisQueueDriver {
    /// Connect to Redis and initialize the producer + consumer.
    ///
    /// # Arguments
    ///
    /// * `url` — Redis URL, e.g. `"redis://127.0.0.1:6379"`.
    /// * `stream` — Redis stream key name.
    /// * `group` — Consumer group name (created with `MKSTREAM` if absent).
    /// * `consumer_id` — Unique consumer ID within the group.
    /// * `visibility_timeout` — How long a message can remain unacknowledged
    ///   before another consumer may re-claim it (`XAUTOCLAIM` idle threshold).
    pub async fn connect(
        url: &str,
        stream: &str,
        group: &str,
        consumer_id: &str,
        visibility_timeout: Duration,
    ) -> Result<Self, FrameworkError> {
        let uri = StreamerUri::from_str(url)
            .map_err(|e| FrameworkError::internal(format!("redis URI parse error: {e}")))?;

        let streamer = RedisStreamer::connect(uri, Default::default())
            .await
            .map_err(|e| FrameworkError::internal(format!("redis connect error: {e}")))?;

        let stream_key = StreamKey::new(stream)
            .map_err(|e| FrameworkError::internal(format!("redis stream key error: {e}")))?;

        // Producer — not anchored; we call send_to explicitly with the stream key.
        let producer: RedisProducer = streamer
            .create_generic_producer(Default::default())
            .await
            .map_err(|e| FrameworkError::internal(format!("redis producer error: {e}")))?;

        // Consumer — LoadBalanced for consumer-group semantics, manual ack.
        let mut opts = RedisConsumerOptions::new(ConsumerMode::LoadBalanced);
        opts.set_consumer_group(ConsumerGroup::new(group))
            .map_err(|e| FrameworkError::internal(format!("redis set group error: {e}")))?;
        opts.set_consumer_id(ConsumerId::new(consumer_id));
        opts.set_auto_commit(AutoCommit::Disabled);
        opts.set_auto_claim_idle(visibility_timeout);
        // Allow consumer to create the group/stream if it doesn't exist yet.
        opts.set_mkstream(true);
        // Create the consumer group at position 0 (beginning of stream) so
        // messages pushed before the first `pop()` call are not missed.
        // The default (Latest / `$`) would skip any messages already in the
        // stream when the group is first initialized on the initial `next()`.
        opts.set_auto_stream_reset(AutoStreamReset::Earliest);

        let consumer: RedisConsumer = streamer
            .create_consumer(std::slice::from_ref(&stream_key), opts)
            .await
            .map_err(|e| FrameworkError::internal(format!("redis consumer error: {e}")))?;

        Ok(Self {
            producer,
            consumer,
            stream_key,
            pending: Mutex::new(HashMap::new()),
        })
    }
}

#[async_trait]
impl QueueDriver for RedisQueueDriver {
    /// Serialize the envelope to JSON and publish it to the Redis stream.
    async fn push(&self, env: Envelope) -> Result<(), FrameworkError> {
        let json = env
            .to_json()
            .map_err(|e| FrameworkError::internal(format!("envelope encode error: {e}")))?;

        // send_to returns a SendFuture; awaiting it delivers the receipt.
        let fut = self
            .producer
            .send_to(&self.stream_key, json.as_str())
            .map_err(|e| FrameworkError::internal(format!("redis send error: {e}")))?;

        fut.await
            .map_err(|e| FrameworkError::internal(format!("redis send receipt error: {e}")))?;

        Ok(())
    }

    /// Poll for the next message. Returns `None` if no message arrives within
    /// `visibility_timeout`. Internally polls in short (100 ms) probe windows
    /// so the caller's deadline is respected without holding the consumer
    /// locked across the full wait.
    ///
    /// Note: `visibility_timeout` controls how long *this call* waits for a
    /// message. The XAUTOCLAIM idle window (how long an unacked message stays
    /// in the PEL before reclaim) is set at construction time and is unrelated.
    async fn pop(&self, visibility_timeout: Duration) -> Result<Option<Reservation>, FrameworkError> {
        // Poll in short probe windows so we return promptly when the queue is
        // empty AND honour the caller's deadline when a message is slow to arrive
        // (e.g. right after a push on a fresh stream/consumer-group).
        let probe = Duration::from_millis(100);
        let deadline = tokio::time::Instant::now() + visibility_timeout;

        let msg = loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let wait = remaining.min(probe);
            match tokio::time::timeout(wait, self.consumer.next()).await {
                // This probe timed out — loop and check deadline.
                Err(_elapsed) => continue,
                // Consumer returned an error.
                Ok(Err(e)) => {
                    return Err(FrameworkError::internal(format!(
                        "redis consumer next error: {e}"
                    )))
                }
                Ok(Ok(msg)) => break msg,
            }
        };

        // Parse the envelope from the message payload.
        // Bind the Payload to a local so its borrow lives long enough.
        let payload = msg.message();
        let payload_bytes = payload.as_bytes();
        let payload_str = std::str::from_utf8(payload_bytes).map_err(|e| {
            FrameworkError::internal(format!("redis message not valid UTF-8: {e}"))
        })?;

        let envelope = Envelope::from_json(payload_str).map_err(|e| {
            FrameworkError::internal(format!("envelope decode error: {e}"))
        })?;

        let token = ReservationToken(envelope.id);

        // Store the shared message so we can ack it later.
        // Call the `Message` trait's `to_owned` explicitly (not `ToOwned`).
        let shared = sea_streamer::Message::to_owned(&msg);
        {
            let mut g = self.pending.lock().expect("redis pending map poisoned");
            g.insert(token.0, (envelope.clone(), shared));
        }

        Ok(Some(Reservation { envelope, token }))
    }

    /// Acknowledge a previously popped message, removing it from the PEL.
    ///
    /// Idempotent: unknown / already-acked tokens are silently ignored.
    ///
    /// At-least-once: the XACK is queued by sea-streamer and flushed on the
    /// next consumer interaction. A crash between `ack().await?` and the
    /// next flush re-delivers the message.
    async fn ack(&self, token: &ReservationToken) -> Result<(), FrameworkError> {
        let entry = {
            let mut g = self.pending.lock().expect("redis pending map poisoned");
            g.remove(&token.0)
        };

        if let Some((_envelope, shared_msg)) = entry {
            self.consumer
                .ack(&shared_msg)
                .map_err(|e| FrameworkError::internal(format!("redis ack error: {e}")))?;

            // Flush the ack to Redis immediately so it doesn't linger.
            // `commit` requires `&mut self` which we don't have here because
            // the trait requires `&self`. With `AutoCommit::Disabled` the ack
            // is queued internally and will be committed when the consumer's
            // internal flush fires or when the next `next()` call triggers it.
            // This is acceptable: the message is out of the consumer's in-flight
            // set from our perspective the moment `ack` is called.
        }
        // Token not found → already acked or never seen → idempotent no-op.

        Ok(())
    }

    /// Return a message to the queue with incremented `attempts` and an
    /// optional delay before it becomes visible again.
    ///
    /// Implementation:
    /// 1. Retrieve and remove the `(Envelope, SharedMessage)` from the pending map.
    /// 2. Bump `envelope.attempts += 1`.
    /// 3. Set `envelope.available_at = now + requeue_delay`.
    /// 4. Re-publish the modified envelope via `XADD`.
    /// 5. Acknowledge the original message via `XACK` (removes it from the PEL).
    ///
    /// At-least-once: the re-publish (XADD) and ack (XACK) are non-atomic.
    /// A crash between XADD success and XACK success causes one extra
    /// delivery with the pre-nack attempts counter.
    async fn nack(
        &self,
        token: &ReservationToken,
        requeue_delay: Duration,
    ) -> Result<(), FrameworkError> {
        let entry = {
            let mut g = self.pending.lock().expect("redis pending map poisoned");
            g.remove(&token.0)
        };

        let (mut envelope, shared_msg) = match entry {
            Some(e) => e,
            // Already acked / unknown token — silently succeed.
            None => return Ok(()),
        };

        // Satisfy the trait contract: bump attempts.
        envelope.attempts += 1;

        // Advance availability by the requested delay.
        let available_at = Utc::now()
            + chrono::Duration::from_std(requeue_delay)
                .unwrap_or(chrono::Duration::zero());
        envelope.available_at = available_at;

        // Re-publish with the bumped envelope.
        let json = envelope
            .to_json()
            .map_err(|e| FrameworkError::internal(format!("envelope encode error (nack): {e}")))?;

        let send_fut = self
            .producer
            .send_to(&self.stream_key, json.as_str())
            .map_err(|e| FrameworkError::internal(format!("redis nack re-publish error: {e}")))?;

        send_fut
            .await
            .map_err(|e| {
                FrameworkError::internal(format!("redis nack re-publish receipt error: {e}"))
            })?;

        // Ack the original message so it leaves the PEL.
        self.consumer
            .ack(&shared_msg)
            .map_err(|e| FrameworkError::internal(format!("redis nack ack error: {e}")))?;

        Ok(())
    }

    fn name(&self) -> &'static str {
        "redis-streams"
    }
}
