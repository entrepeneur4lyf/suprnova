//! QueueDriver trait — placeholder, fleshed out in Task 6.

use crate::error::FrameworkError;
use crate::queue::envelope::Envelope;
use async_trait::async_trait;

#[async_trait]
#[allow(dead_code)]
pub trait QueueDriver: Send + Sync {
    /// Enqueue a fully-formed envelope. Drivers MUST NOT mutate the envelope.
    async fn push(&self, env: Envelope) -> Result<(), FrameworkError>;
}
