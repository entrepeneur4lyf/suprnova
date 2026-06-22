//! Mailgun HTTP transport.
//!
//! POSTs to `https://api.<region>.mailgun.net/v3/<domain>/messages` with
//! `Authorization: Basic base64("api:<key>")`. The body is form-encoded when
//! the message has no attachments and `multipart/form-data` otherwise —
//! Mailgun's form-encoded path doesn't accept file uploads, so attachments
//! force the multipart switch.

use crate::error::FrameworkError;
use crate::mail::address::Address;
use crate::mail::http_provider::{err, read_error_body, shared_client};
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;

const DEFAULT_ENDPOINT: &str = "https://api.mailgun.net";

/// Mailgun HTTP transport. Authenticates via HTTP-basic with the
/// account API key and POSTs form data to
/// `<endpoint>/v3/<domain>/messages`.
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

/// Reject a caller-supplied header name that would inject into multipart
/// `Content-Disposition` or corrupt a form-encoded body. CR, LF, and NUL
/// are the injection characters; any name containing them is rejected.
fn validate_header_name(name: &str) -> Result<(), FrameworkError> {
    if name.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
        return Err(FrameworkError::param(format!(
            "Mailgun: header name contains illegal character (CR, LF, or NUL): {name:?}"
        )));
    }
    Ok(())
}

/// Build the common Mailgun field set shared by the form-encoded and
/// multipart code paths. Kept as a single function so the two paths can't
/// drift on field names or order.
///
/// Mailgun field name conventions:
/// * `o:tag` — tag (repeatable; we send one per tag)
/// * `o:tracking` etc — option params (not surfaced here)
/// * `v:<name>` — message variables (Mailgun's metadata mechanism)
/// * `h:<header-name>` — custom MIME headers
/// * `h:X-Priority` — used by Mailgun for explicit priority
fn build_form_fields(msg: &OutgoingMessage) -> Result<Vec<(String, String)>, FrameworkError> {
    let mut form: Vec<(String, String)> = Vec::with_capacity(32);
    form.push(("from".into(), msg.from.to_string()));
    form.push(("to".into(), join(&msg.to)));
    if !msg.cc.is_empty() {
        form.push(("cc".into(), join(&msg.cc)));
    }
    if !msg.bcc.is_empty() {
        form.push(("bcc".into(), join(&msg.bcc)));
    }
    if !msg.reply_to.is_empty() {
        form.push(("h:Reply-To".into(), join(&msg.reply_to)));
    }
    form.push(("subject".into(), msg.subject.clone()));
    if let Some(h) = &msg.html {
        form.push(("html".into(), h.clone()));
    }
    if let Some(t) = &msg.text {
        form.push(("text".into(), t.clone()));
    }
    for tag in &msg.tags {
        form.push(("o:tag".into(), tag.clone()));
    }
    for (k, v) in &msg.metadata {
        form.push((format!("v:{k}"), v.clone()));
    }
    for (k, v) in &msg.headers {
        validate_header_name(k)?;
        form.push((format!("h:{k}"), v.clone()));
    }
    if let Some(p) = msg.priority {
        form.push(("h:X-Priority".into(), p.to_string()));
    }
    if let Some(rp) = &msg.return_path {
        form.push(("h:Return-Path".into(), rp.to_string()));
    }
    Ok(form)
}

#[async_trait]
impl MailTransport for MailgunMailTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        let fields = build_form_fields(msg)?;

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
            let body = read_error_body(resp).await;
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
    use crate::mail::transport::OutgoingMessage;

    fn base_msg() -> OutgoingMessage {
        use crate::mail::address::Address;
        OutgoingMessage {
            from: Address::new("sender@example.com"),
            to: vec![Address::new("to@example.com")],
            cc: vec![],
            bcc: vec![],
            reply_to: vec![],
            subject: "Test".into(),
            html: None,
            text: Some("hello".into()),
            tags: vec![],
            metadata: std::collections::BTreeMap::new(),
            headers: vec![],
            attachments: vec![],
            priority: None,
            return_path: None,
        }
    }

    #[test]
    fn header_name_with_crlf_is_rejected() {
        let mut msg = base_msg();
        msg.headers = vec![("X-Bad\r\nInjected".into(), "value".into())];
        assert!(
            build_form_fields(&msg).is_err(),
            "CRLF in header name must be rejected"
        );
    }

    #[test]
    fn header_name_with_lf_is_rejected() {
        let mut msg = base_msg();
        msg.headers = vec![("X-Bad\nHeader".into(), "value".into())];
        assert!(
            build_form_fields(&msg).is_err(),
            "LF in header name must be rejected"
        );
    }

    #[test]
    fn header_name_with_nul_is_rejected() {
        let mut msg = base_msg();
        msg.headers = vec![("X-Bad\x00Header".into(), "value".into())];
        assert!(
            build_form_fields(&msg).is_err(),
            "NUL in header name must be rejected"
        );
    }

    #[test]
    fn clean_header_names_are_accepted() {
        let mut msg = base_msg();
        msg.headers = vec![("X-Custom-Header".into(), "some value".into())];
        let fields = build_form_fields(&msg).expect("clean header name must succeed");
        assert!(fields.iter().any(|(k, _)| k == "h:X-Custom-Header"));
    }

    #[test]
    fn new_uses_us_endpoint() {
        let t = MailgunMailTransport::new("key", "example.com");
        assert_eq!(t.endpoint, "https://api.mailgun.net");
        assert_eq!(t.url(), "https://api.mailgun.net/v3/example.com/messages");
    }

    #[test]
    fn with_endpoint_supports_eu() {
        let t =
            MailgunMailTransport::with_endpoint("k", "example.com", "https://api.eu.mailgun.net");
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
        let t =
            MailgunMailTransport::with_endpoint("k", "example.com", "https://api.eu.mailgun.net/");
        assert_eq!(t.endpoint, "https://api.eu.mailgun.net");
        assert_eq!(
            t.url(),
            "https://api.eu.mailgun.net/v3/example.com/messages"
        );
    }
}
