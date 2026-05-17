//! AWS SES (SendEmail v2 API) transport with sigv4-signed requests.

use crate::error::FrameworkError;
use crate::mail::address::Address;
use crate::mail::http_provider::{err, shared_client};
use crate::mail::transport::{MailTransport, OutgoingMessage};
use async_trait::async_trait;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
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
        let region_s: String = region.into();
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
        let url = if endpoint.contains("/outbound-emails") {
            endpoint.to_string()
        } else {
            format!("{endpoint}/v2/email/outbound-emails")
        };
        Self {
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: region.into(),
            endpoint: url,
        }
    }
}

// SES v2 SendEmail request shape:
// { "FromEmailAddress": "...",
//   "Destination": { "ToAddresses": [..], "CcAddresses": [..], "BccAddresses": [..] },
//   "Content": { "Simple": { "Subject": { "Data": "..." },
//                            "Body": { "Html": { "Data": "..." }, "Text": { "Data": "..." } } } },
//   "ReplyToAddresses": [..] }
#[derive(Serialize)]
struct SesBody<'a> {
    #[serde(rename = "FromEmailAddress")]
    from_email_address: String,
    #[serde(rename = "Destination")]
    destination: SesDestination,
    #[serde(rename = "ReplyToAddresses", skip_serializing_if = "Vec::is_empty")]
    reply_to_addresses: Vec<String>,
    #[serde(rename = "Content")]
    content: SesContent<'a>,
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
struct SesContent<'a> {
    #[serde(rename = "Simple")]
    simple: SesSimple<'a>,
}

#[derive(Serialize)]
struct SesSimple<'a> {
    #[serde(rename = "Subject")]
    subject: SesData<'a>,
    #[serde(rename = "Body")]
    body: SesBodyContent<'a>,
}

#[derive(Serialize)]
struct SesData<'a> {
    #[serde(rename = "Data")]
    data: &'a str,
}

#[derive(Serialize)]
struct SesBodyContent<'a> {
    #[serde(rename = "Html", skip_serializing_if = "Option::is_none")]
    html: Option<SesData<'a>>,
    #[serde(rename = "Text", skip_serializing_if = "Option::is_none")]
    text: Option<SesData<'a>>,
}

fn addrs_only(a: &[Address]) -> Vec<String> {
    a.iter().map(|x| x.to_string()).collect()
}

fn uri_host(endpoint: &str) -> String {
    url::Url::parse(endpoint)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

#[async_trait]
impl MailTransport for SesMailTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        let body = SesBody {
            from_email_address: msg.from.to_string(),
            destination: SesDestination {
                to: addrs_only(&msg.to),
                cc: addrs_only(&msg.cc),
                bcc: addrs_only(&msg.bcc),
            },
            reply_to_addresses: addrs_only(&msg.reply_to),
            content: SesContent {
                simple: SesSimple {
                    subject: SesData { data: &msg.subject },
                    body: SesBodyContent {
                        html: msg.html.as_deref().map(|h| SesData { data: h }),
                        text: msg.text.as_deref().map(|t| SesData { data: t }),
                    },
                },
            },
        };
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| FrameworkError::internal(format!("SES encode: {e}")))?;

        // Build the sigv4 identity + signing params.
        let identity = aws_credential_types::Credentials::new(
            self.access_key.clone(),
            self.secret_key.clone(),
            /* session_token = */ None,
            /* expiry = */ None,
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

        // Pre-build an `http::Request` so headers can be sorted and signed.
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

        // Apply signing instructions to the reqwest request.
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
    fn with_endpoint_appends_path_when_missing() {
        let t = SesMailTransport::with_endpoint("AK", "SK", "us-west-2", "https://proxy.example");
        assert_eq!(
            t.endpoint,
            "https://proxy.example/v2/email/outbound-emails"
        );
    }

    #[test]
    fn with_endpoint_preserves_full_path() {
        let t = SesMailTransport::with_endpoint(
            "AK",
            "SK",
            "us-west-2",
            "https://proxy.example/v2/email/outbound-emails",
        );
        assert_eq!(
            t.endpoint,
            "https://proxy.example/v2/email/outbound-emails"
        );
    }

    #[test]
    fn uri_host_strips_scheme_and_path() {
        assert_eq!(
            uri_host("https://email.us-east-1.amazonaws.com/v2/email/outbound-emails"),
            "email.us-east-1.amazonaws.com"
        );
        assert_eq!(uri_host("http://127.0.0.1:38291/v2/email/outbound-emails"), "127.0.0.1");
    }
}
