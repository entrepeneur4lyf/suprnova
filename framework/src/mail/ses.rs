//! AWS SES (SendEmail v2 API) transport with sigv4-signed requests.
//!
//! Plain messages (no attachments) ride the `Content.Simple` JSON path.
//! Anything with attachments is rendered to RFC 5322 MIME via lettre,
//! base64-encoded, and sent as `Content.Raw.Data` — the only SES path
//! that supports attachments.

use crate::error::FrameworkError;
use crate::mail::address::Address;
use crate::mail::http_provider::{err, shared_client};
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;
use aws_sigv4::http_request::{SignableBody, SignableRequest, SigningSettings, sign};
use aws_sigv4::sign::v4::SigningParams;
use lettre::message::{
    Attachment as LettreAttachment, Mailbox, Message, MultiPart, SinglePart, header::ContentType,
};
use serde::Serialize;
use std::time::SystemTime;

pub struct SesMailTransport {
    access_key: String,
    secret_key: String,
    region: String,
    endpoint: String,
}

impl SesMailTransport {
    pub fn new(
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
        region: impl Into<String>,
    ) -> Self {
        let region_s: String = region.into().to_lowercase();
        let endpoint = format!("https://email.{region_s}.amazonaws.com/v2/email/outbound-emails");
        Self {
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: region_s,
            endpoint,
        }
    }

    pub fn with_endpoint(
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
        region: impl Into<String>,
        endpoint: impl AsRef<str>,
    ) -> Self {
        let endpoint = endpoint.as_ref().trim_end_matches('/');
        let url = if endpoint.ends_with("/outbound-emails") {
            endpoint.to_string()
        } else {
            format!("{endpoint}/v2/email/outbound-emails")
        };
        Self {
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: region.into().to_lowercase(),
            endpoint: url,
        }
    }
}

#[derive(Serialize)]
struct SesBody {
    #[serde(rename = "FromEmailAddress")]
    from_email_address: String,
    #[serde(rename = "Destination")]
    destination: SesDestination,
    #[serde(rename = "ReplyToAddresses", skip_serializing_if = "Vec::is_empty")]
    reply_to_addresses: Vec<String>,
    #[serde(rename = "Content")]
    content: SesContent,
}

#[derive(Serialize)]
struct SesDestination {
    #[serde(rename = "ToAddresses", skip_serializing_if = "Vec::is_empty")]
    to: Vec<String>,
    #[serde(rename = "CcAddresses", skip_serializing_if = "Vec::is_empty")]
    cc: Vec<String>,
    #[serde(rename = "BccAddresses", skip_serializing_if = "Vec::is_empty")]
    bcc: Vec<String>,
}

#[derive(Serialize)]
enum SesContent {
    #[serde(rename = "Simple")]
    Simple(SesSimple),
    #[serde(rename = "Raw")]
    Raw(SesRaw),
}

#[derive(Serialize)]
struct SesSimple {
    #[serde(rename = "Subject")]
    subject: SesData,
    #[serde(rename = "Body")]
    body: SesBodyContent,
}

#[derive(Serialize)]
struct SesRaw {
    #[serde(rename = "Data")]
    data: String,
}

#[derive(Serialize)]
struct SesData {
    #[serde(rename = "Data")]
    data: String,
}

#[derive(Serialize)]
struct SesBodyContent {
    #[serde(rename = "Html", skip_serializing_if = "Option::is_none")]
    html: Option<SesData>,
    #[serde(rename = "Text", skip_serializing_if = "Option::is_none")]
    text: Option<SesData>,
}

fn addrs_only(a: &[Address]) -> Vec<String> {
    a.iter().map(|x| x.to_string()).collect()
}

fn uri_host(endpoint: &str) -> String {
    url::Url::parse(endpoint)
        .ok()
        .map(|u| match u.port() {
            Some(p) => format!("{}:{}", u.host_str().unwrap_or(""), p),
            None => u.host_str().unwrap_or("").to_string(),
        })
        .unwrap_or_default()
}

fn address_to_mailbox(a: &Address) -> Result<Mailbox, FrameworkError> {
    let parsed: lettre::Address = a
        .email
        .parse()
        .map_err(|e| FrameworkError::internal(format!("SES parse address {}: {e}", a.email)))?;
    Ok(Mailbox::new(a.name.clone(), parsed))
}

fn build_mime(msg: &OutgoingMessage) -> Result<Vec<u8>, FrameworkError> {
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

    let mut mixed = MultiPart::mixed().multipart(alternative);
    for att in &msg.attachments {
        let ct: ContentType = att.content_type.parse().map_err(|e| {
            FrameworkError::internal(format!(
                "SES attachment content-type {}: {e}",
                att.content_type
            ))
        })?;
        mixed = mixed
            .singlepart(LettreAttachment::new(att.filename.clone()).body(att.content.clone(), ct));
    }

    let email = builder
        .multipart(mixed)
        .map_err(|e| FrameworkError::internal(format!("SES build mime: {e}")))?;
    Ok(email.formatted())
}

#[async_trait]
impl MailTransport for SesMailTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        let content = if msg.attachments.is_empty() {
            SesContent::Simple(SesSimple {
                subject: SesData {
                    data: msg.subject.clone(),
                },
                body: SesBodyContent {
                    html: msg.html.clone().map(|h| SesData { data: h }),
                    text: msg.text.clone().map(|t| SesData { data: t }),
                },
            })
        } else {
            let mime = build_mime(msg)?;
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&mime);
            SesContent::Raw(SesRaw { data: encoded })
        };

        let body = SesBody {
            from_email_address: msg.from.to_string(),
            destination: SesDestination {
                to: addrs_only(&msg.to),
                cc: addrs_only(&msg.cc),
                bcc: addrs_only(&msg.bcc),
            },
            reply_to_addresses: addrs_only(&msg.reply_to),
            content,
        };
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| FrameworkError::internal(format!("SES encode: {e}")))?;

        let identity = aws_credential_types::Credentials::new(
            self.access_key.clone(),
            self.secret_key.clone(),
            None,
            None,
            "suprnova-mail",
        )
        .into();
        let settings = SigningSettings::default();
        let signing_params = SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name("ses")
            .time(SystemTime::now())
            .settings(settings)
            .build()
            .map_err(|e| FrameworkError::internal(format!("SES sigv4 params: {e}")))?
            .into();

        let request_builder = http::Request::builder()
            .method("POST")
            .uri(&self.endpoint)
            .header("content-type", "application/json")
            .header("host", uri_host(&self.endpoint));
        let request = request_builder
            .body(body_bytes.clone())
            .map_err(|e| FrameworkError::internal(format!("SES http build: {e}")))?;

        let signable = SignableRequest::new(
            "POST",
            &self.endpoint,
            request
                .headers()
                .iter()
                .map(|(k, v)| (k.as_str(), v.to_str().unwrap_or(""))),
            SignableBody::Bytes(&body_bytes),
        )
        .map_err(|e| FrameworkError::internal(format!("SES signable: {e}")))?;

        let signing_output = sign(signable, &signing_params)
            .map_err(|e| FrameworkError::internal(format!("SES sign: {e}")))?;
        let (signing_instructions, _signature) = signing_output.into_parts();

        let mut rb = shared_client()
            .post(&self.endpoint)
            .header("content-type", "application/json");
        for (name, value) in signing_instructions.headers() {
            rb = rb.header(name, value);
        }

        let resp = rb
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| FrameworkError::internal(format!("SES transport: {e}")))?;

        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let body = resp.text().await.unwrap_or_default();
            return Err(err("SES", status, body));
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "ses"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_region_aware_endpoint() {
        let t = SesMailTransport::new("AK", "SK", "us-east-1");
        assert_eq!(
            t.endpoint,
            "https://email.us-east-1.amazonaws.com/v2/email/outbound-emails"
        );
        assert_eq!(t.region, "us-east-1");
    }

    #[test]
    fn new_lowercases_uppercase_region() {
        let t = SesMailTransport::new("AK", "SK", "US-EAST-1");
        assert_eq!(t.region, "us-east-1");
        assert_eq!(
            t.endpoint,
            "https://email.us-east-1.amazonaws.com/v2/email/outbound-emails"
        );
    }

    #[test]
    fn with_endpoint_lowercases_uppercase_region() {
        let t = SesMailTransport::with_endpoint("AK", "SK", "EU-West-2", "https://x.example");
        assert_eq!(t.region, "eu-west-2");
    }

    #[test]
    fn with_endpoint_appends_path_when_missing() {
        let t = SesMailTransport::with_endpoint("AK", "SK", "us-west-2", "https://proxy.example");
        assert_eq!(t.endpoint, "https://proxy.example/v2/email/outbound-emails");
    }

    #[test]
    fn with_endpoint_preserves_full_path() {
        let t = SesMailTransport::with_endpoint(
            "AK",
            "SK",
            "us-west-2",
            "https://proxy.example/v2/email/outbound-emails",
        );
        assert_eq!(t.endpoint, "https://proxy.example/v2/email/outbound-emails");
    }

    #[test]
    fn with_endpoint_appends_for_paths_with_outbound_emails_substring() {
        // Regression: `contains("/outbound-emails")` would have skipped a base
        // URL like `/outbound-emails-archive/api`. `ends_with` is correct.
        let t = SesMailTransport::with_endpoint(
            "AK",
            "SK",
            "us-west-2",
            "https://x.example/outbound-emails-archive/api",
        );
        assert_eq!(
            t.endpoint,
            "https://x.example/outbound-emails-archive/api/v2/email/outbound-emails"
        );
    }

    #[test]
    fn uri_host_preserves_port_when_non_default() {
        assert_eq!(
            uri_host("https://email.us-east-1.amazonaws.com/v2/email/outbound-emails"),
            "email.us-east-1.amazonaws.com"
        );
        assert_eq!(
            uri_host("http://127.0.0.1:38291/v2/email/outbound-emails"),
            "127.0.0.1:38291"
        );
        assert_eq!(
            uri_host("https://example.com:8443/v2/email/outbound-emails"),
            "example.com:8443"
        );
    }
}
