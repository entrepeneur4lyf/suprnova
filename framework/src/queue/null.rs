//! Null queue driver — discards every push, returns nothing.
//!
//! Mirrors Laravel's `NullQueue`. Useful for code paths that want to keep
//! the `Queue::push` call site without firing the side effect (e.g.
//! `QUEUE_DRIVER=null` in CI when the work being queued is what's under
//! test, not the queueing itself).

use crate::error::FrameworkError;
use crate::queue::driver::{QueueDriver, Reservation, ReservationToken};
use crate::queue::envelope::Envelope;
use async_trait::async_trait;
use std::time::Duration;

#[derive(Default)]
pub struct NullQueueDriver;

impl NullQueueDriver {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl QueueDriver for NullQueueDriver {
    async fn push(&self, _env: Envelope) -> Result<(), FrameworkError> {
        Ok(())
    }

    async fn pop(&self, _vt: Duration) -> Result<Option<Reservation>, FrameworkError> {
        Ok(None)
    }

    async fn ack(&self, _t: &ReservationToken) -> Result<(), FrameworkError> {
        Ok(())
    }

    async fn nack(&self, _t: &ReservationToken, _delay: Duration) -> Result<(), FrameworkError> {
        Ok(())
    }

    async fn size(&self) -> Result<u64, FrameworkError> {
        Ok(0)
    }

    async fn clear(&self) -> Result<u64, FrameworkError> {
        Ok(0)
    }

    fn name(&self) -> &'static str {
        "null"
    }
}
