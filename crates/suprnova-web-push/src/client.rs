//! Web Push HTTP client. POSTs an encrypted payload + VAPID
//! authorization to the subscription's endpoint via reqwest 0.13.

use crate::error::WebPushError;
use crate::payload::{ContentEncoding, Payload};
use crate::vapid::VapidSigner;
use reqwest::Client;
use reqwest::header::{
    AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName,
    HeaderValue,
};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubscriptionInfo {
    pub endpoint: String,
    pub keys: SubscriptionKeys,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}

#[derive(Debug, Clone)]
pub struct PushResponse {
    pub status: u16,
}

#[derive(Debug)]
pub struct WebPushClient {
    http: Client,
    signer: VapidSigner,
    subject: String,
}

impl WebPushClient {
    pub fn new(signer: VapidSigner, subject: impl Into<String>) -> Self {
        Self::with_client(Client::new(), signer, subject)
    }

    pub fn with_client(http: Client, signer: VapidSigner, subject: impl Into<String>) -> Self {
        Self {
            http,
            signer,
            subject: subject.into(),
        }
    }

    pub async fn send(
        &self,
        subscription: &SubscriptionInfo,
        plaintext: &[u8],
        encoding: ContentEncoding,
        ttl_secs: u32,
    ) -> Result<PushResponse, WebPushError> {
        let payload = Payload::encrypt(
            plaintext,
            &subscription.keys.p256dh,
            &subscription.keys.auth,
            encoding,
        )?;

        let audience = audience_of(&subscription.endpoint)?;
        let jwt = self.signer.sign(&audience, &self.subject, 12 * 3600)?;
        let pub_b64 = self.signer.public_key_b64url();

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("vapid t={jwt}, k={pub_b64}"))
                .map_err(|e| WebPushError::Internal(format!("auth header: {e}")))?,
        );
        headers.insert(
            HeaderName::from_static("crypto-key"),
            HeaderValue::from_str(&format!("p256ecdsa={pub_b64}"))
                .map_err(|e| WebPushError::Internal(format!("crypto-key header: {e}")))?,
        );
        headers.insert(HeaderName::from_static("ttl"), HeaderValue::from(ttl_secs));
        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("aes128gcm"));
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(CONTENT_LENGTH, HeaderValue::from(payload.body().len()));

        let resp = self
            .http
            .post(&subscription.endpoint)
            .headers(headers)
            .body(payload.body().to_vec())
            .send()
            .await?;

        let status = resp.status().as_u16();
        match status {
            201 | 202 | 204 => Ok(PushResponse { status }),
            404 | 410 => Err(WebPushError::SubscriptionGone),
            _ => {
                let body = resp.text().await.unwrap_or_default();
                Err(WebPushError::PushServiceRejected { status, body })
            }
        }
    }
}

fn audience_of(endpoint: &str) -> Result<String, WebPushError> {
    let url =
        Url::parse(endpoint).map_err(|e| WebPushError::Internal(format!("endpoint url: {e}")))?;
    let mut out = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));
    if let Some(p) = url.port() {
        out.push(':');
        out.push_str(&p.to_string());
    }
    Ok(out)
}
