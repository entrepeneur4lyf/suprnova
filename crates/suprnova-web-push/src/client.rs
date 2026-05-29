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
use std::time::Duration;
use url::Url;

/// Default per-request timeout applied to the [`Client`] constructed by
/// [`WebPushClient::new`]. A slow or hostile push service must not be able to
/// tie up a calling task indefinitely. Callers wanting a different policy
/// build their own [`Client`] and pass it to [`WebPushClient::with_client`].
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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

/// Validation policy applied to a subscription endpoint before the client
/// POSTs to it.
///
/// Subscription endpoints are user-derived data — the browser receives the URL
/// from a remote push service when a user subscribes, and the application
/// stores it. A maliciously stored subscription can point the push HTTP POST
/// anywhere reachable, turning the push sender into an SSRF gadget.
///
/// [`EndpointPolicy::Strict`] is the production default: HTTPS-only, named
/// hosts only, common SSRF targets rejected. [`EndpointPolicy::AllowAny`]
/// disables validation and exists for tests that hit a local mock server
/// (wiremock, http loopback). Do not use [`EndpointPolicy::AllowAny`] in
/// production code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EndpointPolicy {
    /// HTTPS-only, named hosts only, IP literals + common SSRF targets
    /// (localhost, `.local`/`.internal`/`.test`/`.example`/`.invalid` TLDs,
    /// cloud metadata hosts) rejected. The production default.
    #[default]
    Strict,
    /// Accept any URL the underlying [`Client`] accepts. Intended for tests
    /// against controlled mock servers — DO NOT use in production.
    AllowAny,
}

#[derive(Debug)]
pub struct WebPushClient {
    http: Client,
    signer: VapidSigner,
    subject: String,
    endpoint_policy: EndpointPolicy,
}

impl WebPushClient {
    /// Build a client with a sensibly-defaulted HTTP transport.
    ///
    /// The default transport applies a 30-second per-request timeout so a
    /// slow or hostile push service cannot tie up the caller indefinitely.
    /// Callers needing a different transport policy build their own
    /// [`Client`] and use [`Self::with_client`] instead.
    pub fn new(signer: VapidSigner, subject: impl Into<String>) -> Self {
        let http = Client::builder()
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .build()
            .expect("default reqwest Client builds without TLS/proxy config");
        Self::with_client(http, signer, subject)
    }

    /// Build a client wrapping a caller-supplied [`Client`].
    ///
    /// Use this when the default transport (a 30 s timeout) is the wrong
    /// policy — for example, when wiring through a corporate proxy, pinning
    /// TLS, or applying a different timeout.
    pub fn with_client(http: Client, signer: VapidSigner, subject: impl Into<String>) -> Self {
        Self {
            http,
            signer,
            subject: subject.into(),
            endpoint_policy: EndpointPolicy::default(),
        }
    }

    /// Override the subscription-endpoint validation policy.
    ///
    /// Defaults to [`EndpointPolicy::Strict`]. Switching to
    /// [`EndpointPolicy::AllowAny`] disables the HTTPS / named-host / blocked-
    /// TLD checks and is intended only for test code that targets a controlled
    /// mock endpoint. Production code must not call this with `AllowAny`.
    pub fn with_endpoint_policy(mut self, policy: EndpointPolicy) -> Self {
        self.endpoint_policy = policy;
        self
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

        // Parse + validate the endpoint under the active policy. Strict (the
        // production default) rejects anything other than an HTTPS URL whose
        // host is a named, non-SSRF-target hostname. AllowAny still parses the
        // URL so a malformed string fails fast.
        let endpoint_url = parse_endpoint(&subscription.endpoint)?;
        if self.endpoint_policy == EndpointPolicy::Strict {
            validate_strict_endpoint(&endpoint_url)?;
        }

        let audience = audience_of_url(&endpoint_url);
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
            .post(endpoint_url)
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

fn parse_endpoint(endpoint: &str) -> Result<Url, WebPushError> {
    Url::parse(endpoint).map_err(|e| WebPushError::Internal(format!("endpoint url: {e}")))
}

/// Reject endpoints that can't plausibly reach a real push service.
///
/// Enforced rules:
/// - scheme must be `https` (RFC 8030 requires it; HTTP would also bypass TLS)
/// - URL must have a host component
/// - host must NOT be an IP literal — real push services use named hosts; an
///   IP literal in a subscription almost always indicates an SSRF probe or a
///   tampered store
/// - host must NOT be one of the common SSRF targets (cloud metadata services,
///   `localhost`, RFC-2606 reserved TLDs: `.localhost`, `.local`, `.internal`,
///   `.test`, `.example`, `.invalid`)
///
/// What this does NOT do: resolve the hostname and check the resolved IP
/// against a private-CIDR blocklist. That requires a DNS resolver and runtime
/// IP-range matching to be effective against DNS-rebinding-style attacks; it
/// is a sensible follow-on layer for stricter threat models. Callers needing
/// that can apply it via [`WebPushClient::with_client`] using a hardened
/// [`Client`] (custom resolver, etc.).
fn validate_strict_endpoint(url: &Url) -> Result<(), WebPushError> {
    if url.scheme() != "https" {
        return Err(WebPushError::Internal(format!(
            "subscription endpoint must use https (got scheme '{}')",
            url.scheme()
        )));
    }

    // Use the typed `url::Host` so IPv4 / IPv6 are detected regardless of
    // URL syntax. `host_str` for IPv6 returns the bracketed form (e.g.
    // `"[::1]"`), which does NOT parse as `std::net::IpAddr` — relying on
    // that parse alone leaks IPv6-literal hosts past the guard.
    let domain = match url.host() {
        None => {
            return Err(WebPushError::Internal(
                "subscription endpoint has no host".into(),
            ));
        }
        Some(url::Host::Ipv4(addr)) => {
            return Err(WebPushError::Internal(format!(
                "subscription endpoint host '{addr}' is an IP literal; real push services use named hosts"
            )));
        }
        Some(url::Host::Ipv6(addr)) => {
            return Err(WebPushError::Internal(format!(
                "subscription endpoint host '[{addr}]' is an IP literal; real push services use named hosts"
            )));
        }
        Some(url::Host::Domain(d)) => d,
    };

    let host_normalized = domain.to_ascii_lowercase();
    let host_trimmed = host_normalized.trim_end_matches('.');
    // Exact-match blocklist — cloud metadata host names that resolve to
    // link-local addresses (169.254.169.254 etc.) on the host network.
    const BLOCKED_EXACT: &[&str] = &[
        "localhost",
        "metadata.google.internal",
        "metadata.aws.internal",
        "metadata.azure.com",
        "instance-data",
    ];
    // RFC-2606 reserved + commonly internal TLDs.
    const BLOCKED_SUFFIXES: &[&str] = &[
        ".localhost",
        ".local",
        ".internal",
        ".test",
        ".example",
        ".invalid",
    ];
    if BLOCKED_EXACT.contains(&host_trimmed)
        || BLOCKED_SUFFIXES.iter().any(|s| host_trimmed.ends_with(s))
    {
        return Err(WebPushError::Internal(format!(
            "subscription endpoint host '{domain}' is not a valid push service host"
        )));
    }
    Ok(())
}

fn audience_of_url(url: &Url) -> String {
    let mut out = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));
    if let Some(p) = url.port() {
        out.push(':');
        out.push_str(&p.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_rejects_non_https_scheme() {
        let url = Url::parse("http://fcm.googleapis.com/push/abc").unwrap();
        let err = validate_strict_endpoint(&url).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("https"),
            "non-https must be rejected, got: {msg}"
        );
    }

    #[test]
    fn strict_rejects_ip_literal_hosts() {
        for bad in [
            "https://127.0.0.1/push",
            "https://10.0.0.1/push",
            "https://192.168.1.1/push",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/push",
            "https://[fe80::1]/push",
        ] {
            let url = Url::parse(bad).unwrap();
            assert!(
                validate_strict_endpoint(&url).is_err(),
                "IP-literal host must be rejected: {bad}"
            );
        }
    }

    #[test]
    fn strict_rejects_blocked_hosts_and_tlds() {
        for bad in [
            "https://localhost/push",
            "https://localhost./push",
            "https://service.local/push",
            "https://anything.internal/push",
            "https://my.test/push",
            "https://example.example/push",
            "https://name.invalid/push",
            "https://metadata.google.internal/computeMetadata/v1/",
            "https://metadata.aws.internal/latest/",
            "https://metadata.azure.com/metadata/instance",
            "https://app.localhost/push",
            "https://LOCALHOST/push",
        ] {
            let url = Url::parse(bad).unwrap();
            assert!(
                validate_strict_endpoint(&url).is_err(),
                "blocked host must be rejected: {bad}"
            );
        }
    }

    #[test]
    fn strict_accepts_real_push_services() {
        for ok in [
            "https://fcm.googleapis.com/fcm/send/abc",
            "https://updates.push.services.mozilla.com/wpush/v2/abc",
            "https://web.push.apple.com/abc",
            "https://push.example.org/abc",
            "https://my-app.cloudfront.net/push",
        ] {
            let url = Url::parse(ok).unwrap();
            assert!(
                validate_strict_endpoint(&url).is_ok(),
                "legitimate push host must be accepted: {ok}"
            );
        }
    }

    #[test]
    fn strict_rejects_url_with_no_host() {
        // A path-only URL has no host.
        let url = Url::parse("file:///etc/passwd").unwrap();
        let err = validate_strict_endpoint(&url).unwrap_err();
        let msg = err.to_string();
        // Either the scheme or host check fires first; both are acceptable.
        assert!(
            msg.contains("https") || msg.contains("host"),
            "URL with no host must be rejected: {msg}"
        );
    }
}
