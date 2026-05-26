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

    /// Driver name for logs/admin. Default uses type name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}
