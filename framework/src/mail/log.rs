//! Log mail transport — emits a `tracing::info!` per send and discards.
//! Useful for local dev where you want to see what mail WOULD send.

use crate::error::FrameworkError;
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;

/// Dev-time transport that emits a `tracing::info!` line per dispatch
/// and discards the message. Useful for inspecting what mail *would*
/// send without contacting an upstream provider.
///
/// The log line carries the envelope (from / to / subject) **and the
/// rendered plain-text body** (Laravel's `log` mailer likewise writes the
/// full message to the log). The body matters in dev: verification and
/// password-reset links live in it, and with this transport the console is
/// the only place they surface. HTML bodies are summarised by byte length —
/// markup soup in a terminal helps nobody, and every framework-shipped
/// mailable renders a text alternative.
#[derive(Default)]
pub struct LogMailTransport;

impl LogMailTransport {
    /// Construct a fresh log transport.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MailTransport for LogMailTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        let to: Vec<String> = msg.to.iter().map(|a| a.email.clone()).collect();
        let text = msg.text.as_deref().unwrap_or("(no text body)");
        let html = msg
            .html
            .as_ref()
            .map(|h| format!("{} bytes", h.len()))
            .unwrap_or_else(|| "none".into());
        tracing::info!(
            from = %msg.from.email,
            to = ?to,
            subject = %msg.subject,
            html = %html,
            text = %text,
            "mail (log driver): would send"
        );
        Ok(())
    }
    fn name(&self) -> &'static str {
        "log"
    }
}
