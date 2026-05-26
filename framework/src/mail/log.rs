//! Log mail transport — emits a `tracing::info!` per send and discards.
//! Useful for local dev where you want to see what mail WOULD send.

use crate::error::FrameworkError;
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;

#[derive(Default)]
pub struct LogMailTransport;

impl LogMailTransport {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MailTransport for LogMailTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        let to: Vec<String> = msg.to.iter().map(|a| a.email.clone()).collect();
        tracing::info!(
            from = %msg.from.email,
            to = ?to,
            subject = %msg.subject,
            "mail (log driver): would send"
        );
        Ok(())
    }
    fn name(&self) -> &'static str {
        "log"
    }
}
