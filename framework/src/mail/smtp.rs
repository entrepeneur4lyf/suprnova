//! Lettre-backed SMTP transport. Tokio + rustls.

use crate::error::FrameworkError;
use crate::mail::address::Address;
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;
use lettre::message::header::{HeaderName, HeaderValue};
use lettre::message::{
    Attachment as LettreAttachment, Mailbox, Message, MultiPart, SinglePart, header::ContentType,
};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

pub struct SmtpMailTransport {
    inner: AsyncSmtpTransport<Tokio1Executor>,
}

impl SmtpMailTransport {
    /// STARTTLS submission. Pass `587` for the standard submission port,
    /// or a non-default port for relays that use one (gateway, proxy).
    pub fn starttls(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
    ) -> Result<Self, FrameworkError> {
        let creds = Credentials::new(user.to_string(), password.to_string());
        let inner = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
            .map_err(|e| FrameworkError::internal(format!("smtp starttls: {e}")))?
            .port(port)
            .credentials(creds)
            .build();
        Ok(Self { inner })
    }

    /// TLS-wrapped SMTP. Pass `465` for the canonical implicit-TLS port,
    /// or a non-default port for a custom relay.
    pub fn tls(host: &str, port: u16, user: &str, password: &str) -> Result<Self, FrameworkError> {
        let creds = Credentials::new(user.to_string(), password.to_string());
        let inner = AsyncSmtpTransport::<Tokio1Executor>::relay(host)
            .map_err(|e| FrameworkError::internal(format!("smtp tls relay: {e}")))?
            .port(port)
            .credentials(creds)
            .build();
        Ok(Self { inner })
    }

    /// Plain unencrypted SMTP (for local Mailpit/MailHog dev only).
    pub fn unencrypted(host: &str, port: u16) -> Result<Self, FrameworkError> {
        let inner = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
            .port(port)
            .build();
        Ok(Self { inner })
    }
}

#[async_trait]
impl MailTransport for SmtpMailTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        let mut builder = Message::builder()
            .from(address_to_mailbox(&msg.from)?)
            .subject(&msg.subject);

        for a in &msg.to {
            builder = builder.to(address_to_mailbox(a)?);
        }
        for a in &msg.cc {
            builder = builder.cc(address_to_mailbox(a)?);
        }
        for a in &msg.bcc {
            builder = builder.bcc(address_to_mailbox(a)?);
        }
        for a in &msg.reply_to {
            builder = builder.reply_to(address_to_mailbox(a)?);
        }

        // Tags / metadata / priority / return-path / custom headers
        // ride on RFC 5322 headers so a backend MTA can route on them.
        for (name, value) in &msg.headers {
            builder = builder.raw_header(custom_header(name, value)?);
        }
        if let Some(p) = msg.priority {
            builder = builder.raw_header(custom_header("X-Priority", &p.to_string())?);
            // Importance: 1-2 = High, 3 = Normal, 4-5 = Low.
            let imp = match p {
                1..=2 => "High",
                4..=5 => "Low",
                _ => "Normal",
            };
            builder = builder.raw_header(custom_header("Importance", imp)?);
        }
        for t in &msg.tags {
            builder = builder.raw_header(custom_header("X-Tag", t)?);
        }
        for (k, v) in &msg.metadata {
            builder = builder.raw_header(custom_header(
                format!("X-Metadata-{k}").as_str(),
                v.as_str(),
            )?);
        }
        if let Some(rp) = &msg.return_path {
            builder = builder.raw_header(custom_header("Return-Path", &rp.to_string())?);
        }

        let multipart = build_body(msg)?;
        let email = builder
            .multipart(multipart)
            .map_err(|e| FrameworkError::internal(format!("smtp build message: {e}")))?;

        self.inner
            .send(email)
            .await
            .map_err(|e| FrameworkError::internal(format!("smtp send: {e}")))?;
        Ok(())
    }

    fn name(&self) -> &'static str {
        "smtp"
    }
}

fn custom_header(name: &str, value: &str) -> Result<HeaderValue, FrameworkError> {
    let header_name = HeaderName::new_from_ascii(name.to_string())
        .map_err(|e| FrameworkError::internal(format!("smtp header name {name}: {e}")))?;
    Ok(HeaderValue::new(header_name, value.to_string()))
}

fn address_to_mailbox(a: &Address) -> Result<Mailbox, FrameworkError> {
    let parsed: lettre::Address = a
        .email
        .parse()
        .map_err(|e| FrameworkError::internal(format!("smtp parse address {}: {e}", a.email)))?;
    Ok(Mailbox::new(a.name.clone(), parsed))
}

fn build_body(msg: &OutgoingMessage) -> Result<MultiPart, FrameworkError> {
    let mut alternative = MultiPart::alternative().build();
    if let Some(text) = &msg.text {
        alternative = alternative.singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_PLAIN)
                .body(text.clone()),
        );
    }
    if let Some(html) = &msg.html {
        alternative = alternative.singlepart(
            SinglePart::builder()
                .header(ContentType::TEXT_HTML)
                .body(html.clone()),
        );
    }

    if msg.attachments.is_empty() {
        return Ok(alternative);
    }

    let mut mixed = MultiPart::mixed().multipart(alternative);
    for att in &msg.attachments {
        let ct: ContentType = att.content_type.parse().map_err(|e| {
            FrameworkError::internal(format!(
                "smtp attachment content-type {}: {e}",
                att.content_type
            ))
        })?;
        let part = LettreAttachment::new(att.filename.clone()).body(att.content.clone(), ct);
        mixed = mixed.singlepart(part);
    }
    Ok(mixed)
}
