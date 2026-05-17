//! Mailgun HTTP transport.
//!
//! POSTs to `https://api.<region>.mailgun.net/v3/<domain>/messages` with
//! `Authorization: Basic base64("api:<key>")`. The body is form-encoded when
//! the message has no attachments and `multipart/form-data` otherwise —
//! Mailgun's form-encoded path doesn't accept file uploads, so attachments
//! force the multipart switch.

use crate::error::FrameworkError;
use crate::mail::address::Address;
use crate::mail::http_provider::{err, shared_client};
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;

const DEFAULT_ENDPOINT: &str = "https://api.mailgun.net";

pub struct MailgunMailTransport {
    api_key: String,
    domain: String,
    /// Base URL (no trailing slash, no `/v3/...` path) — e.g.
    /// `https://api.mailgun.net` or `https://api.eu.mailgun.net`.
    endpoint: String,
}

impl MailgunMailTransport {
    /// US-region transport (`api.mailgun.net`).
    pub fn new(api_key: impl Into<String>, domain: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            domain: domain.into(),
            endpoint: DEFAULT_ENDPOINT.into(),
        }
    }

    /// Override the base URL. Use `https://api.eu.mailgun.net` for EU-region
    /// accounts, or a test mock server's URI.
    pub fn with_endpoint(
        api_key: impl Into<String>,
        domain: impl Into<String>,
        endpoint: impl AsRef<str>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            domain: domain.into(),
            endpoint: endpoint.as_ref().trim_end_matches('/').to_string(),
        }
    }

    fn url(&self) -> String {
        format!("{}/v3/{}/messages", self.endpoint, self.domain)
    }
}

fn join(addrs: &[Address]) -> String {
    addrs
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build the common Mailgun field set shared by the form-encoded and
/// multipart code paths. Kept as a single function so the two paths can't
/// drift on field names or order.
fn build_form_fields(msg: &OutgoingMessage) -> Vec<(&'static str, String)> {
    let mut form: Vec<(&'static str, String)> = Vec::with_capacity(16);
    form.push(("from", msg.from.to_string()));
    form.push(("to", join(&msg.to)));
    if !msg.cc.is_empty() {
        form.push(("cc", join(&msg.cc)));
    }
    if !msg.bcc.is_empty() {
        form.push(("bcc", join(&msg.bcc)));
    }
    if !msg.reply_to.is_empty() {
        form.push(("h:Reply-To", join(&msg.reply_to)));
    }
    form.push(("subject", msg.subject.clone()));
    if let Some(h) = &msg.html {
        form.push(("html", h.clone()));
    }
    if let Some(t) = &msg.text {
        form.push(("text", t.clone()));
    }
    form
}

#[async_trait]
impl MailTransport for MailgunMailTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        let fields = build_form_fields(msg);

        let resp = if msg.attachments.is_empty() {
            // Form-encoded path: smaller wire payload, no multipart overhead.
            shared_client()
                .post(self.url())
                .basic_auth("api", Some(&self.api_key))
                .form(&fields)
                .send()
                .await
        } else {
            // multipart/form-data path: Mailgun's form-encoded API does not
            // accept file uploads; attachments must ride a multipart body.
            let mut form = reqwest::multipart::Form::new();
            for (key, value) in fields {
                form = form.text(key, value);
            }
            for att in &msg.attachments {
                let part = reqwest::multipart::Part::bytes(att.content.clone())
                    .file_name(att.filename.clone())
                    .mime_str(&att.content_type)
                    .map_err(|e| {
                        FrameworkError::internal(format!(
                            "Mailgun attachment mime ({}): {e}",
                            att.content_type
                        ))
                    })?;
                // Per Mailgun's API the form field name for each attachment
                // is the literal string `attachment` (repeated for multiples).
                form = form.part("attachment", part);
            }
            shared_client()
                .post(self.url())
                .basic_auth("api", Some(&self.api_key))
                .multipart(form)
                .send()
                .await
        }
        .map_err(|e| FrameworkError::internal(format!("Mailgun transport: {e}")))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Err(err("Mailgun", status, body));
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "mailgun"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_us_endpoint() {
        let t = MailgunMailTransport::new("key", "example.com");
        assert_eq!(t.endpoint, "https://api.mailgun.net");
        assert_eq!(t.url(), "https://api.mailgun.net/v3/example.com/messages");
    }

    #[test]
    fn with_endpoint_supports_eu() {
        let t = MailgunMailTransport::with_endpoint(
            "k",
            "example.com",
            "https://api.eu.mailgun.net",
        );
        assert_eq!(t.endpoint, "https://api.eu.mailgun.net");
        assert_eq!(
            t.url(),
            "https://api.eu.mailgun.net/v3/example.com/messages"
        );
    }

    #[test]
    fn with_endpoint_trims_trailing_slash() {
        // `https://api.eu.mailgun.net/` (with trailing slash) must be
        // normalised to the no-slash form so the join doesn't produce
        // `//v3/...`.
        let t = MailgunMailTransport::with_endpoint(
            "k",
            "example.com",
            "https://api.eu.mailgun.net/",
        );
        assert_eq!(t.endpoint, "https://api.eu.mailgun.net");
        assert_eq!(
            t.url(),
            "https://api.eu.mailgun.net/v3/example.com/messages"
        );
    }
}
