//! In-memory mail transport — captures messages without sending.

use crate::error::FrameworkError;
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;
use std::sync::Mutex;

/// Test-only [`MailTransport`] that retains every dispatched message
/// in-process. Backs [`MailFake`](crate::mail::MailFake).
#[derive(Default)]
pub struct InMemoryMailTransport {
    sent: Mutex<Vec<OutgoingMessage>>,
}

impl InMemoryMailTransport {
    /// Construct a fresh, empty in-memory transport.
    pub fn new() -> Self {
        Self::default()
    }

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
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(msg.clone());
        Ok(())
    }
    fn name(&self) -> &'static str {
        "in-memory"
    }
}
