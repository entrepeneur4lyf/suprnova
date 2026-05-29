//! Mail dispatch events — `MessageSending` (pre-send observability) and
//! `MessageSent` (post-send observability).
//!
//! These mirror Laravel's `Illuminate\Mail\Events\MessageSending` and
//! `MessageSent`. The dispatcher's `events->until()` cancellation model
//! used by Laravel is NOT exposed here — Suprnova's event dispatcher
//! delivers observers without a short-circuit return channel. Listeners
//! that need to suppress a send should refuse at the Mailable layer
//! (override `render_html`/`render_text` to return an error) or wrap
//! the `MailBuilder::send` call with their own gate.

use crate::error::FrameworkError;
use crate::events::{Event, EventFacade};
use crate::mail::address::Address;
use crate::mail::transport::OutgoingMessage;

/// Fired immediately BEFORE `MailTransport::send` for the rendered
/// `OutgoingMessage`. Useful for last-mile observability (audit
/// logging, sampled tracing) and best-effort metrics.
#[derive(Debug, Clone)]
pub struct MessageSending {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub subject: String,
    pub has_html: bool,
    pub has_text: bool,
    pub attachment_count: usize,
    pub tags: Vec<String>,
}

impl Event for MessageSending {
    fn event_name() -> &'static str {
        "Suprnova\\Mail\\MessageSending"
    }
}

/// Fired immediately AFTER a successful `MailTransport::send`. Fired only
/// when the transport returned `Ok(())` — failed sends do not emit this
/// event (the `warn`-level `mail send failed` line from the dispatch
/// span is the failure-side record).
#[derive(Debug, Clone)]
pub struct MessageSent {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub subject: String,
    pub has_html: bool,
    pub has_text: bool,
    pub attachment_count: usize,
    pub tags: Vec<String>,
}

impl Event for MessageSent {
    fn event_name() -> &'static str {
        "Suprnova\\Mail\\MessageSent"
    }
}

impl From<&OutgoingMessage> for MessageSending {
    fn from(m: &OutgoingMessage) -> Self {
        Self {
            from: m.from.clone(),
            to: m.to.clone(),
            cc: m.cc.clone(),
            bcc: m.bcc.clone(),
            reply_to: m.reply_to.clone(),
            subject: m.subject.clone(),
            has_html: m.html.is_some(),
            has_text: m.text.is_some(),
            attachment_count: m.attachments.len(),
            tags: m.tags.clone(),
        }
    }
}

impl From<&OutgoingMessage> for MessageSent {
    fn from(m: &OutgoingMessage) -> Self {
        Self {
            from: m.from.clone(),
            to: m.to.clone(),
            cc: m.cc.clone(),
            bcc: m.bcc.clone(),
            reply_to: m.reply_to.clone(),
            subject: m.subject.clone(),
            has_html: m.html.is_some(),
            has_text: m.text.is_some(),
            attachment_count: m.attachments.len(),
            tags: m.tags.clone(),
        }
    }
}

pub(crate) async fn fire_sending(msg: &OutgoingMessage) {
    let _ = dispatch_or_log::<MessageSending>(msg.into()).await;
}

pub(crate) async fn fire_sent(msg: &OutgoingMessage) {
    let _ = dispatch_or_log::<MessageSent>(msg.into()).await;
}

async fn dispatch_or_log<E: Event>(event: E) -> Result<(), FrameworkError> {
    // Mail event dispatch is best-effort: a missing listener registry
    // or a failing observer must NOT block the underlying send. We
    // forward through `EventFacade::dispatch` (which routes through the
    // test fake when active) and swallow any listener-side error —
    // observability events are not part of the mail send contract.
    let result = EventFacade::dispatch(event).await;
    if let Err(e) = &result {
        tracing::warn!(error = %e, "mail event listener failed");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mail::address::Address;
    use crate::mail::transport::OutgoingMessage;

    fn make_msg() -> OutgoingMessage {
        let mut m = OutgoingMessage::new(Address::new("from@example.com"));
        m.to = vec![Address::new("to@example.org")];
        m.subject = "hi".into();
        m.text = Some("body".into());
        m.tags = vec!["welcome".into()];
        m
    }

    #[test]
    fn message_sending_from_outgoing_copies_shape() {
        let msg = make_msg();
        let ev = MessageSending::from(&msg);
        assert_eq!(ev.from.email, "from@example.com");
        assert_eq!(ev.to[0].email, "to@example.org");
        assert_eq!(ev.subject, "hi");
        assert!(ev.has_text);
        assert!(!ev.has_html);
        assert_eq!(ev.tags, vec!["welcome".to_string()]);
        assert_eq!(ev.attachment_count, 0);
    }

    #[test]
    fn message_sent_from_outgoing_copies_shape() {
        let msg = make_msg();
        let ev = MessageSent::from(&msg);
        assert_eq!(ev.from.email, "from@example.com");
        assert_eq!(ev.subject, "hi");
    }

    #[test]
    fn event_names_are_stable() {
        assert_eq!(
            MessageSending::event_name(),
            "Suprnova\\Mail\\MessageSending"
        );
        assert_eq!(MessageSent::event_name(), "Suprnova\\Mail\\MessageSent");
    }
}
