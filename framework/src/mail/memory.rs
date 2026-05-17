//! In-memory mail transport — captures messages without sending.

use crate::error::FrameworkError;
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;
use std::sync::Mutex;

#[derive(Default)]
pub struct InMemoryMailTransport {
    sent: Mutex<Vec<OutgoingMessage>>,
}

impl InMemoryMailTransport {
    pub fn new() -> Self { Self::default() }

    /// Snapshot of every message sent through this transport.
    pub fn captured(&self) -> Vec<OutgoingMessage> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Clear the captured-message buffer.
    pub fn clear(&self) {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

#[async_trait]
impl MailTransport for InMemoryMailTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).push(msg.clone());
        Ok(())
    }
    fn name(&self) -> &'static str { "in-memory" }
}
