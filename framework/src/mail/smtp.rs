//! Lettre-backed SMTP transport. Tokio + rustls.

use crate::error::FrameworkError;
use crate::mail::address::Address;
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;
use lettre::message::{
    header::ContentType, Attachment as LettreAttachment, Mailbox, Message, MultiPart, SinglePart,
};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

pub struct SmtpMailTransport {
    inner: AsyncSmtpTransport<Tokio1Executor>,
}

impl SmtpMailTransport {
    /// STARTTLS relay on the standard port (587). Use this for production.
    pub async fn starttls(host: &str, user: &str, password: &str) -> Result<Self, FrameworkError> {
        let creds = Credentials::new(user.to_string(), password.to_string());
        let inner = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
            .map_err(|e| FrameworkError::internal(format!("smtp starttls: {e}")))?
            .credentials(creds)
            .build();
        Ok(Self { inner })
    }

    /// TLS-wrapped relay on port 465.
    pub async fn tls(host: &str, user: &str, password: &str) -> Result<Self, FrameworkError> {
        let creds = Credentials::new(user.to_string(), password.to_string());
        let inner = AsyncSmtpTransport::<Tokio1Executor>::relay(host)
            .map_err(|e| FrameworkError::internal(format!("smtp tls relay: {e}")))?
            .credentials(creds)
            .build();
        Ok(Self { inner })
    }

    /// Plain unencrypted SMTP (for local Mailpit/MailHog dev only).
    pub async fn unencrypted(host: &str, port: u16) -> Result<Self, FrameworkError> {
        let inner = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
            .port(port)
            .build();
        Ok(Self { inner })
    }

    pub fn into_inner(self) -> AsyncSmtpTransport<Tokio1Executor> {
        self.inner
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

    fn name(&self) -> &'static str { "smtp" }
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
        let ct: ContentType = att
            .content_type
            .parse()
            .map_err(|e| {
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
