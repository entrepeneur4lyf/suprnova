//! MailTransport trait + rendered-message representation.

use crate::error::FrameworkError;
use crate::mail::address::{Address, Attachment};
use async_trait::async_trait;
use tracing::Instrument;

/// A fully-rendered outgoing message — what transports receive.
#[derive(Debug, Clone)]
pub struct OutgoingMessage {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub subject: String,
    pub html: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<Attachment>,
}

#[async_trait]
pub trait MailTransport: Send + Sync {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError>;
    fn name(&self) -> &'static str { std::any::type_name::<Self>() }
}

/// Telemetry-wrapped send. Opens a `mail.send` info span carrying the
/// transport name and message shape (recipient counts, body kinds,
/// attachment count), then delegates to `transport.send`. On completion
/// emits one event with `duration_ms` — `info` on success, `warn` on
/// error — so an observability backend can plot mail latency without
/// the transports themselves needing to know.
///
/// All call sites that ship to a [`MailTransport`] should go through this
/// helper: [`MailBuilder::send`](crate::mail::MailBuilder::send), the
/// queue worker in [`SendMailJob`](crate::mail::SendMailJob), and the
/// notification [`MailChannel`](crate::notifications::channels::mail::MailChannel).
/// Routing every dispatch through one entrypoint guarantees one span
/// schema regardless of how the message was produced.
///
/// Fields are kept deliberately minimal (the message *shape*, not its
/// content). The point at this layer is *presence* — Phase 8 (admin
/// observability) layers the richer per-domain attributes on top.
pub async fn dispatch_with_telemetry(
    transport: &dyn MailTransport,
    msg: &OutgoingMessage,
) -> Result<(), FrameworkError> {
    let span = tracing::info_span!(
        "mail.send",
        transport = transport.name(),
        to_count = msg.to.len(),
        cc_count = msg.cc.len(),
        bcc_count = msg.bcc.len(),
        has_html = msg.html.is_some(),
        has_text = msg.text.is_some(),
        attachment_count = msg.attachments.len(),
    );
    async move {
        let start = std::time::Instant::now();
        let result = transport.send(msg).await;
        let duration_ms = start.elapsed().as_millis() as u64;
        match &result {
            Ok(()) => tracing::info!(duration_ms, "mail sent"),
            Err(e) => tracing::warn!(duration_ms, error = %e, "mail send failed"),
        }
        result
    }
    .instrument(span)
    .await
}
