//! Resend HTTP transport. POSTs JSON to https://api.resend.com/emails with
//! `Authorization: Bearer <api-key>`.

use crate::error::FrameworkError;
use crate::mail::address::Address;
use crate::mail::http_provider::{err, shared_client};
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;
use serde::Serialize;

const DEFAULT_ENDPOINT: &str = "https://api.resend.com/emails";

pub struct ResendMailTransport {
    api_key: String,
    endpoint: String,
}

impl ResendMailTransport {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            endpoint: DEFAULT_ENDPOINT.into(),
        }
    }

    pub fn with_endpoint(api_key: impl Into<String>, endpoint: impl AsRef<str>) -> Self {
        // Trim trailing slash first so `https://x.example/emails/` is detected
        // as already-terminated and we don't double-append.
        let e = endpoint.as_ref().trim_end_matches('/');
        // `ends_with` (not `contains`) — a base URL like `/emails-archive/api`
        // only *contains* the substring but is not the Resend endpoint, so we
        // must still append.
        let url = if e.ends_with("/emails") {
            e.to_string()
        } else {
            format!("{e}/emails")
        };
        Self {
            api_key: api_key.into(),
            endpoint: url,
        }
    }
}

#[derive(Serialize)]
struct RsBody<'a> {
    from: String,
    to: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cc: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    bcc: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    reply_to: Vec<String>,
    subject: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    html: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<RsAttachment<'a>>,
}

#[derive(Serialize)]
struct RsAttachment<'a> {
    filename: &'a str,
    content: String, // base64
    content_type: &'a str,
}

fn addr_str(a: &Address) -> String {
    a.to_string()
}

#[async_trait]
impl MailTransport for ResendMailTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        use base64::Engine;
        let attachments: Vec<RsAttachment> = msg
            .attachments
            .iter()
            .map(|a| RsAttachment {
                filename: &a.filename,
                content: base64::engine::general_purpose::STANDARD.encode(&a.content),
                content_type: &a.content_type,
            })
            .collect();

        let body = RsBody {
            from: addr_str(&msg.from),
            to: msg.to.iter().map(addr_str).collect(),
            cc: msg.cc.iter().map(addr_str).collect(),
            bcc: msg.bcc.iter().map(addr_str).collect(),
            reply_to: msg.reply_to.iter().map(addr_str).collect(),
            subject: &msg.subject,
            html: msg.html.as_deref(),
            text: msg.text.as_deref(),
            attachments,
        };

        let resp = shared_client()
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| FrameworkError::internal(format!("Resend transport: {e}")))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Err(err("Resend", status, body));
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "resend"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_default_endpoint() {
        let t = ResendMailTransport::new("k");
        assert_eq!(t.endpoint, DEFAULT_ENDPOINT);
    }

    #[test]
    fn with_endpoint_appends_emails_path_when_missing() {
        let t = ResendMailTransport::with_endpoint("k", "https://proxy.example.com");
        assert_eq!(t.endpoint, "https://proxy.example.com/emails");
    }

    #[test]
    fn with_endpoint_preserves_terminal_emails_path() {
        let t = ResendMailTransport::with_endpoint("k", "https://proxy.example.com/emails");
        assert_eq!(t.endpoint, "https://proxy.example.com/emails");
    }

    #[test]
    fn with_endpoint_trims_trailing_slash_before_suffix_check() {
        // `https://x/emails/` must be detected as already-terminal (after
        // trim), not double-appended.
        let t = ResendMailTransport::with_endpoint("k", "https://proxy.example.com/emails/");
        assert_eq!(t.endpoint, "https://proxy.example.com/emails");
    }

    #[test]
    fn with_endpoint_appends_for_paths_with_emails_substring() {
        // Regression: `contains("/emails")` would have skipped a base URL
        // like `/emails-archive/api`. `ends_with` is correct.
        let t = ResendMailTransport::with_endpoint("k", "https://x.example/emails-archive/api");
        assert_eq!(t.endpoint, "https://x.example/emails-archive/api/emails");
    }
}
