//! Postmark HTTP transport. POSTs JSON to https://api.postmarkapp.com/email.

use crate::error::FrameworkError;
use crate::mail::address::Address;
use crate::mail::http_provider::{err, shared_client};
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;
use serde::Serialize;
use std::collections::BTreeMap;

const DEFAULT_ENDPOINT: &str = "https://api.postmarkapp.com/email";

pub struct PostmarkMailTransport {
    token: String,
    endpoint: String,
}

impl PostmarkMailTransport {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            endpoint: DEFAULT_ENDPOINT.into(),
        }
    }

    pub fn with_endpoint(token: impl Into<String>, endpoint: impl AsRef<str>) -> Self {
        let endpoint = endpoint.as_ref().trim_end_matches('/');
        let url = if endpoint.ends_with("/email") {
            endpoint.to_string()
        } else {
            format!("{endpoint}/email")
        };
        Self {
            token: token.into(),
            endpoint: url,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_default_endpoint() {
        let t = PostmarkMailTransport::new("tok");
        assert_eq!(t.endpoint, DEFAULT_ENDPOINT);
    }

    #[test]
    fn with_endpoint_appends_email_path_when_missing() {
        let t = PostmarkMailTransport::with_endpoint("tok", "https://proxy.example.com");
        assert_eq!(t.endpoint, "https://proxy.example.com/email");
    }

    #[test]
    fn with_endpoint_preserves_terminal_email_path() {
        let t = PostmarkMailTransport::with_endpoint("tok", "https://proxy.example.com/email");
        assert_eq!(t.endpoint, "https://proxy.example.com/email");
    }

    #[test]
    fn with_endpoint_appends_email_for_paths_that_only_contain_email_substring() {
        // Guard against the regression where `contains("/email")` would have
        // skipped the suffix for a base URL like `/email-archive/api`.
        let t =
            PostmarkMailTransport::with_endpoint("tok", "https://example.com/email-archive/api");
        assert_eq!(t.endpoint, "https://example.com/email-archive/api/email");
    }
}

#[derive(Serialize)]
struct PostmarkBody<'a> {
    #[serde(rename = "From")]
    from: String,
    #[serde(rename = "To")]
    to: String,
    #[serde(rename = "Cc", skip_serializing_if = "String::is_empty")]
    cc: String,
    #[serde(rename = "Bcc", skip_serializing_if = "String::is_empty")]
    bcc: String,
    #[serde(rename = "ReplyTo", skip_serializing_if = "String::is_empty")]
    reply_to: String,
    #[serde(rename = "Subject")]
    subject: &'a str,
    #[serde(rename = "HtmlBody", skip_serializing_if = "Option::is_none")]
    html_body: Option<&'a str>,
    #[serde(rename = "TextBody", skip_serializing_if = "Option::is_none")]
    text_body: Option<&'a str>,
    #[serde(rename = "Attachments", skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<PostmarkAttachment<'a>>,
    /// Postmark accepts ONE tag per message; we send the first.
    #[serde(rename = "Tag", skip_serializing_if = "Option::is_none")]
    tag: Option<&'a str>,
    #[serde(rename = "Metadata", skip_serializing_if = "BTreeMap::is_empty")]
    metadata: BTreeMap<String, String>,
    #[serde(rename = "Headers", skip_serializing_if = "Vec::is_empty")]
    headers: Vec<PmHeader<'a>>,
}

#[derive(Serialize)]
struct PmHeader<'a> {
    #[serde(rename = "Name")]
    name: &'a str,
    #[serde(rename = "Value")]
    value: &'a str,
}

#[derive(Serialize)]
struct PostmarkAttachment<'a> {
    #[serde(rename = "Name")]
    name: &'a str,
    #[serde(rename = "Content")]
    content: String, // base64
    #[serde(rename = "ContentType")]
    content_type: &'a str,
}

fn join(addrs: &[Address]) -> String {
    addrs
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[async_trait]
impl MailTransport for PostmarkMailTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        use base64::Engine;
        let attachments: Vec<PostmarkAttachment> = msg
            .attachments
            .iter()
            .map(|a| PostmarkAttachment {
                name: &a.filename,
                content: base64::engine::general_purpose::STANDARD.encode(&a.content),
                content_type: &a.content_type,
            })
            .collect();

        // Postmark headers are an array of {Name, Value} objects. We
        // build a flat vec from the message's headers plus any
        // priority value (Postmark has no first-class priority field,
        // but a custom `X-Priority` header is conventional).
        let priority_str = msg.priority.map(|p| p.to_string()).unwrap_or_default();
        let mut headers_vec: Vec<PmHeader> = msg
            .headers
            .iter()
            .map(|(n, v)| PmHeader { name: n, value: v })
            .collect();
        if msg.priority.is_some() {
            headers_vec.push(PmHeader {
                name: "X-Priority",
                value: priority_str.as_str(),
            });
        }

        let body = PostmarkBody {
            from: msg.from.to_string(),
            to: join(&msg.to),
            cc: join(&msg.cc),
            bcc: join(&msg.bcc),
            reply_to: join(&msg.reply_to),
            subject: &msg.subject,
            html_body: msg.html.as_deref(),
            text_body: msg.text.as_deref(),
            attachments,
            tag: msg.tags.first().map(|s| s.as_str()),
            metadata: msg.metadata.clone(),
            headers: headers_vec,
        };

        let resp = shared_client()
            .post(&self.endpoint)
            .header("x-postmark-server-token", &self.token)
            .header("accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| FrameworkError::internal(format!("Postmark transport: {e}")))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Err(err("Postmark", status, body));
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "postmark"
    }
}
