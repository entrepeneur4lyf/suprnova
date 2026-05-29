//! Queue driver trait — the contract every backend implements.

use crate::error::FrameworkError;
use crate::queue::envelope::Envelope;
use async_trait::async_trait;
use std::time::Duration;
use uuid::Uuid;

/// Opaque token identifying one reservation of a popped envelope.
/// Workers MUST present this token to `ack` or `nack` the message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReservationToken(pub Uuid);

/// One popped message + its reservation token.
#[derive(Debug, Clone)]
pub struct Reservation {
    pub envelope: Envelope,
    pub token: ReservationToken,
}

#[async_trait]
pub trait QueueDriver: Send + Sync {
    /// Enqueue a fully-formed envelope. Drivers MUST NOT mutate it.
    async fn push(&self, env: Envelope) -> Result<(), FrameworkError>;

    /// Pop the next available envelope, reserving it for `visibility_timeout`.
    /// Returns `None` if no message is available within a short driver-local
    /// poll budget. Drivers MAY block up to ~100ms.
    async fn pop(
        &self,
        visibility_timeout: Duration,
    ) -> Result<Option<Reservation>, FrameworkError>;

    /// Acknowledge successful completion of a reserved message. Drivers MUST
    /// be tolerant of unknown / already-acked tokens (idempotent).
    async fn ack(&self, token: &ReservationToken) -> Result<(), FrameworkError>;

    /// Return a reserved message to the queue with `requeue_delay`.
    ///
    /// **Implementors MUST increment the stored envelope's `attempts`
    /// before re-enqueuing**, so the worker's `attempts >= max_tries`
    /// guard advances correctly across retry cycles. Drivers that store
    /// the envelope server-side (Redis, SQL, etc.) bump on the server;
    /// in-memory drivers bump in their `Inner` map. Failing to bump
    /// causes infinite retry loops.
    ///
    /// Drivers MUST be tolerant of unknown / already-acked tokens (idempotent).
    async fn nack(
        &self,
        token: &ReservationToken,
        requeue_delay: Duration,
    ) -> Result<(), FrameworkError>;

    /// Total count of envelopes currently held by this driver
    /// (pending + delayed + reserved). Mirrors Laravel's
    /// `Queue::size($queue)`.
    ///
    /// Default implementation returns `Err` describing the unsupported
    /// operation — drivers that can answer the count cheaply override.
    async fn size(&self) -> Result<u64, FrameworkError> {
        Err(FrameworkError::internal(format!(
            "queue driver '{}' does not implement size()",
            self.name()
        )))
    }

    /// Count of envelopes whose `available_at <= now` and which are not
    /// currently reserved. Mirrors Laravel's `pendingSize($queue)`.
    /// Defaults to [`size`] minus the reserved/delayed counts.
    async fn pending_size(&self) -> Result<u64, FrameworkError> {
        let total = self.size().await?;
        let reserved = self.reserved_size().await.unwrap_or(0);
        let delayed = self.delayed_size().await.unwrap_or(0);
        Ok(total.saturating_sub(reserved).saturating_sub(delayed))
    }

    /// Count of envelopes whose `available_at > now`. Mirrors
    /// `delayedSize($queue)`.
    async fn delayed_size(&self) -> Result<u64, FrameworkError> {
        Ok(0)
    }

    /// Count of currently-reserved envelopes (popped, not yet acked).
    /// Mirrors `reservedSize($queue)`.
    async fn reserved_size(&self) -> Result<u64, FrameworkError> {
        Ok(0)
    }

    /// Drop every envelope, returning the number removed. Mirrors
    /// `Queue::clear($queue)` and the `ClearableQueue` contract.
    async fn clear(&self) -> Result<u64, FrameworkError> {
        Err(FrameworkError::internal(format!(
            "queue driver '{}' does not implement clear()",
            self.name()
        )))
    }

    /// Push every envelope in one shot. Mirrors `Queue::bulk($jobs, ...)`.
    /// Default implementation pushes serially; backends with native bulk
    /// push (sea-streamer pipeline, DB multi-row insert) may override.
    async fn bulk_push(&self, envs: Vec<Envelope>) -> Result<(), FrameworkError> {
        for env in envs {
            self.push(env).await?;
        }
        Ok(())
    }

    /// Driver name for logs/admin. Default uses type name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}
