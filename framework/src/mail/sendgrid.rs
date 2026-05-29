//! SendGrid v3 HTTP transport. POSTs JSON to https://api.sendgrid.com/v3/mail/send.

use crate::error::FrameworkError;
use crate::mail::address::Address;
use crate::mail::http_provider::{err, shared_client};
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;
use serde::Serialize;
use std::collections::BTreeMap;

const DEFAULT_ENDPOINT: &str = "https://api.sendgrid.com/v3/mail/send";

pub struct SendGridMailTransport {
    api_key: String,
    endpoint: String,
}

impl SendGridMailTransport {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            endpoint: DEFAULT_ENDPOINT.into(),
        }
    }

    pub fn with_endpoint(api_key: impl Into<String>, endpoint: impl AsRef<str>) -> Self {
        // Trim trailing slash first so `https://x.example/v3/mail/send/` is
        // detected as already-terminated and we don't double-append.
        let e = endpoint.as_ref().trim_end_matches('/');
        // `ends_with` (not `contains`) — a base URL like
        // `/v3/mail/send-archive/api` only *contains* the substring but is
        // not the SendGrid endpoint, so we must still append.
        let url = if e.ends_with("/v3/mail/send") {
            e.to_string()
        } else {
            format!("{e}/v3/mail/send")
        };
        Self {
            api_key: api_key.into(),
            endpoint: url,
        }
    }
}

#[derive(Serialize)]
struct SgBody<'a> {
    personalizations: Vec<SgPersonalization>,
    from: SgAddress,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to: Option<SgAddress>,
    subject: &'a str,
    content: Vec<SgContent<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<SgAttachment<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    categories: Vec<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    custom_args: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct SgPersonalization {
    to: Vec<SgAddress>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cc: Vec<SgAddress>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    bcc: Vec<SgAddress>,
}

#[derive(Serialize)]
struct SgAddress {
    email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Serialize)]
struct SgContent<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    value: &'a str,
}

#[derive(Serialize)]
struct SgAttachment<'a> {
    content: String, // base64
    #[serde(rename = "type")]
    kind: &'a str,
    filename: &'a str,
    disposition: &'a str,
}

fn to_sg(addr: &Address) -> SgAddress {
    SgAddress {
        email: addr.email.clone(),
        name: addr.name.clone(),
    }
}

#[async_trait]
impl MailTransport for SendGridMailTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        use base64::Engine;

        // SendGrid v3 enforces RFC 1341 ordering: text/plain MUST precede
        // text/html in the `content` array, or the API returns 400. Do not
        // reorder these pushes.
        let mut content = Vec::with_capacity(2);
        if let Some(t) = &msg.text {
            content.push(SgContent {
                kind: "text/plain",
                value: t,
            });
        }
        if let Some(h) = &msg.html {
            content.push(SgContent {
                kind: "text/html",
                value: h,
            });
        }
        if content.is_empty() {
            return Err(FrameworkError::internal(
                "SendGrid: at least one of html/text required",
            ));
        }

        let attachments: Vec<SgAttachment> = msg
            .attachments
            .iter()
            .map(|a| SgAttachment {
                content: base64::engine::general_purpose::STANDARD.encode(&a.content),
                kind: &a.content_type,
                filename: &a.filename,
                disposition: "attachment",
            })
            .collect();

        // SendGrid v3 `/v3/mail/send` only accepts a single `reply_to`
        // object — unlike Postmark (CSV) or SES (array). If the caller
        // configured multiple addresses we still send the first but
        // surface a warn so the dropped recipients aren't invisible.
        let reply_to = msg.reply_to.first().map(to_sg);
        if msg.reply_to.len() > 1 {
            let dropped: Vec<&str> = msg
                .reply_to
                .iter()
                .skip(1)
                .map(|a| a.email.as_str())
                .collect();
            tracing::warn!(
                kept = %msg.reply_to[0].email,
                dropped = ?dropped,
                "SendGrid v3 supports only one reply_to; additional addresses ignored"
            );
        }

        // SendGrid headers: caller-set headers go directly into the
        // `headers` object. Priority maps to `X-Priority` (SendGrid has
        // no first-class priority knob). Categories carry tags;
        // `custom_args` carries metadata.
        let mut headers: BTreeMap<String, String> = msg
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Some(p) = msg.priority {
            headers.insert("X-Priority".into(), p.to_string());
        }
        let custom_args: BTreeMap<String, String> = msg
            .metadata
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let body = SgBody {
            personalizations: vec![SgPersonalization {
                to: msg.to.iter().map(to_sg).collect(),
                cc: msg.cc.iter().map(to_sg).collect(),
                bcc: msg.bcc.iter().map(to_sg).collect(),
            }],
            from: to_sg(&msg.from),
            reply_to,
            subject: &msg.subject,
            content,
            attachments,
            categories: msg.tags.clone(),
            custom_args,
            headers,
        };

        let resp = shared_client()
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| FrameworkError::internal(format!("SendGrid transport: {e}")))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Err(err("SendGrid", status, body));
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "sendgrid"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_default_endpoint() {
        let t = SendGridMailTransport::new("k");
        assert_eq!(t.endpoint, DEFAULT_ENDPOINT);
    }

    #[test]
    fn with_endpoint_appends_path_when_missing() {
        let t = SendGridMailTransport::with_endpoint("k", "https://proxy.example.com");
        assert_eq!(t.endpoint, "https://proxy.example.com/v3/mail/send");
    }

    #[test]
    fn with_endpoint_preserves_terminal_path() {
        let t = SendGridMailTransport::with_endpoint("k", "https://proxy.example.com/v3/mail/send");
        assert_eq!(t.endpoint, "https://proxy.example.com/v3/mail/send");
    }

    #[test]
    fn with_endpoint_trims_trailing_slash_before_suffix_check() {
        // `https://x/v3/mail/send/` must be detected as already-terminal
        // (after trim), not double-appended.
        let t =
            SendGridMailTransport::with_endpoint("k", "https://proxy.example.com/v3/mail/send/");
        assert_eq!(t.endpoint, "https://proxy.example.com/v3/mail/send");
    }

    #[test]
    fn with_endpoint_appends_for_paths_that_only_contain_send_substring() {
        // Regression: `contains("/v3/mail/send")` would have skipped a base
        // URL like `/v3/mail/send-archive/api`. `ends_with` is correct.
        let t =
            SendGridMailTransport::with_endpoint("k", "https://x.example/v3/mail/send-archive/api");
        assert_eq!(
            t.endpoint,
            "https://x.example/v3/mail/send-archive/api/v3/mail/send"
        );
    }
}
