//! MailTransport trait + rendered-message representation.
//!
//! `OutgoingMessage` is the fully-rendered envelope that every transport
//! receives. The shape was widened beyond just-recipients-and-body to
//! match Laravel's Mailable: per-message `tags`, `metadata`, `priority`,
//! `headers`, and an optional `return_path` (Sender / Bounce-To). The
//! HTTP providers (Postmark, SES, SendGrid, Mailgun, Resend) forward
//! these to their respective JSON / form payloads; SMTP serializes them
//! as `X-PM-Tag` style headers when the provider expects them, or as
//! native MIME headers otherwise.

use crate::error::FrameworkError;
use crate::mail::address::{Address, Attachment};
use async_trait::async_trait;
use std::collections::BTreeMap;
use tracing::Instrument;

/// Priority value matching Laravel/Symfony's `priority()` semantics:
/// `1` is highest priority, `5` is lowest. Setting `priority` on an
/// `OutgoingMessage` causes SMTP to emit the corresponding
/// `X-Priority`/`Importance` headers, and HTTP providers that surface
/// priority through their own field set it accordingly.
pub const PRIORITY_HIGHEST: u8 = 1;
pub const PRIORITY_HIGH: u8 = 2;
pub const PRIORITY_NORMAL: u8 = 3;
pub const PRIORITY_LOW: u8 = 4;
pub const PRIORITY_LOWEST: u8 = 5;

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
    /// Provider tags (Postmark `Tag`, SES `Tags`, SendGrid `categories`,
    /// Mailgun `o:tag`, Resend `tags`). Free-form strings; semantics
    /// depend on the upstream provider.
    pub tags: Vec<String>,
    /// Provider metadata. Postmark `Metadata`, SES `Tags` (k/v),
    /// SendGrid `custom_args`, Mailgun `v:` prefixed variables, Resend
    /// `headers` (provider lacks a metadata field). Ordered for
    /// deterministic serialization in tests.
    pub metadata: BTreeMap<String, String>,
    /// Message priority. `1` = highest, `5` = lowest. None = unset.
    pub priority: Option<u8>,
    /// Custom MIME headers (key, value). Pass-through for transports
    /// that support free-form headers — SMTP, SES (raw), most HTTP
    /// providers via `Headers`.
    pub headers: Vec<(String, String)>,
    /// Return-Path / Bounce-To address. SMTP sets `Return-Path:` and the
    /// MAIL FROM envelope; HTTP providers expose it as `ReturnPath` /
    /// `bounce` depending on the provider.
    pub return_path: Option<Address>,
}

impl OutgoingMessage {
    /// Build an empty `OutgoingMessage` with `from` set. Used internally by
    /// builders and tests where the message is constructed field-by-field.
    pub fn new(from: Address) -> Self {
        Self {
            from,
            to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: String::new(),
            html: None,
            text: None,
            attachments: Vec::new(),
            tags: Vec::new(),
            metadata: BTreeMap::new(),
            priority: None,
            headers: Vec::new(),
            return_path: None,
        }
    }

    /// True when the captured message lists `email` in its `to` block.
    /// Mirrors Laravel's `Mailable::hasTo($address)` semantics — match
    /// on email, ignoring case.
    pub fn has_to(&self, email: &str) -> bool {
        has_email(&self.to, email)
    }

    /// True when the captured message lists `email` in its `cc` block.
    pub fn has_cc(&self, email: &str) -> bool {
        has_email(&self.cc, email)
    }

    /// True when the captured message lists `email` in its `bcc` block.
    pub fn has_bcc(&self, email: &str) -> bool {
        has_email(&self.bcc, email)
    }

    /// True when the captured message lists `email` in its `reply_to` block.
    pub fn has_reply_to(&self, email: &str) -> bool {
        has_email(&self.reply_to, email)
    }

    /// True when the captured message's `from` matches `email`.
    pub fn has_from(&self, email: &str) -> bool {
        self.from.email.eq_ignore_ascii_case(email)
    }

    /// Subject equality. Mirrors `Mailable::hasSubject`.
    pub fn has_subject(&self, subject: &str) -> bool {
        self.subject == subject
    }

    /// True when an attachment with the given filename is present.
    /// Filename match is exact (case-sensitive).
    pub fn has_attachment(&self, filename: &str) -> bool {
        self.attachments.iter().any(|a| a.filename == filename)
    }

    /// True when `tag` is in the tag list (case-sensitive).
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    /// True when the metadata map contains `key`.
    pub fn has_metadata(&self, key: &str) -> bool {
        self.metadata.contains_key(key)
    }

    /// True when the metadata entry under `key` equals `value`.
    pub fn metadata_equals(&self, key: &str, value: &str) -> bool {
        self.metadata.get(key).map(|v| v.as_str()) == Some(value)
    }

    /// True when the custom header list contains `(name, value)` (case-insensitive name).
    pub fn has_header(&self, name: &str, value: &str) -> bool {
        self.headers
            .iter()
            .any(|(n, v)| n.eq_ignore_ascii_case(name) && v == value)
    }
}

fn has_email(list: &[Address], email: &str) -> bool {
    list.iter().any(|a| a.email.eq_ignore_ascii_case(email))
}

#[async_trait]
pub trait MailTransport: Send + Sync {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError>;
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// Telemetry-wrapped send. Opens a `mail.send` info span carrying the
/// transport name and message shape (recipient counts, body kinds,
/// attachment count, tag count, metadata count), then delegates to
/// `transport.send`. On completion emits one event with `duration_ms`
/// — `info` on success, `warn` on error — so an observability backend
/// can plot mail latency without the transports themselves needing to
/// know.
///
/// All call sites that ship to a [`MailTransport`] should go through this
/// helper: [`MailBuilder::send`](crate::mail::MailBuilder::send), the
/// queue worker in [`SendMailJob`](crate::mail::SendMailJob), and the
/// notification [`MailChannel`](crate::notifications::channels::mail::MailChannel).
/// Routing every dispatch through one entrypoint guarantees one span
/// schema regardless of how the message was produced.
///
/// Fields are kept deliberately minimal (the message *shape*, not its
/// content).
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
        tag_count = msg.tags.len(),
        metadata_count = msg.metadata.len(),
        priority = msg.priority.unwrap_or(0),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(email: &str) -> Address {
        Address::new(email)
    }

    fn sample() -> OutgoingMessage {
        let mut m = OutgoingMessage::new(addr("noreply@example.com"));
        m.to = vec![addr("alice@example.org")];
        m.cc = vec![addr("manager@example.com")];
        m.bcc = vec![addr("audit@example.com")];
        m.reply_to = vec![addr("support@example.com")];
        m.subject = "hello".into();
        m.text = Some("body".into());
        m.attachments = vec![Attachment::new("r.csv", b"x".to_vec(), "text/csv")];
        m.tags = vec!["welcome".into(), "transactional".into()];
        m.metadata.insert("order_id".into(), "42".into());
        m.priority = Some(PRIORITY_HIGH);
        m.headers = vec![("X-Campaign".into(), "spring".into())];
        m
    }

    #[test]
    fn has_to_is_case_insensitive() {
        let m = sample();
        assert!(m.has_to("ALICE@example.org"));
        assert!(m.has_to("alice@example.org"));
        assert!(!m.has_to("eve@example.com"));
    }

    #[test]
    fn has_cc_bcc_reply_to_match_on_email_only() {
        let m = sample();
        assert!(m.has_cc("manager@example.com"));
        assert!(m.has_bcc("audit@example.com"));
        assert!(m.has_reply_to("support@example.com"));
        assert!(!m.has_cc("ghost@example.com"));
    }

    #[test]
    fn has_from_subject_attachment_tag_metadata_header() {
        let m = sample();
        assert!(m.has_from("noreply@example.com"));
        assert!(m.has_subject("hello"));
        assert!(m.has_attachment("r.csv"));
        assert!(!m.has_attachment("invoice.pdf"));
        assert!(m.has_tag("welcome"));
        assert!(!m.has_tag("nope"));
        assert!(m.has_metadata("order_id"));
        assert!(m.metadata_equals("order_id", "42"));
        assert!(!m.metadata_equals("order_id", "99"));
        assert!(m.has_header("X-Campaign", "spring"));
        assert!(m.has_header("x-campaign", "spring"));
    }

    #[test]
    fn new_constructs_minimal_message() {
        let m = OutgoingMessage::new(addr("a@b.c"));
        assert_eq!(m.from.email, "a@b.c");
        assert!(m.to.is_empty());
        assert!(m.tags.is_empty());
        assert!(m.metadata.is_empty());
        assert_eq!(m.priority, None);
        assert!(m.headers.is_empty());
        assert!(m.return_path.is_none());
    }
}
