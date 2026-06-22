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

/// Cap on the rejection-body bytes buffered from a push-service error
/// response. Real push services return small, human-readable rejection
/// messages (`"too many requests"`, `"invalid registration token"`); a
/// hostile or buggy endpoint streaming gigabytes of body must not be able
/// to drive the sender's memory growth. Bodies are streamed and read
/// stops once the cap is reached — the underlying connection is dropped
/// without consuming the remainder.
pub(crate) const MAX_ERROR_BODY_BYTES: usize = 8 * 1024;

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
    /// The default transport does not follow redirects (a push service never
    /// issues a 3xx, and following one would bypass endpoint SSRF validation).
    /// Callers needing a different transport policy build their own
    /// [`Client`] and use [`Self::with_client`] instead.
    ///
    /// Returns an error if `subject` is not a VAPID-conformant contact
    /// URI — see [`Self::with_client`] for the validation rules.
    pub fn new(signer: VapidSigner, subject: impl Into<String>) -> Result<Self, WebPushError> {
        let http = Client::builder()
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            // Web Push (RFC 8030) services answer 2xx/4xx/5xx, never 3xx.
            // Following redirects would let a subscription endpoint that
            // passes `validate_strict_endpoint` bounce the request to an
            // internal host / cloud metadata (the initial URL is the only
            // one validated). Disabling redirects closes that SSRF.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("default reqwest Client builds without TLS/proxy config");
        Self::with_client(http, signer, subject)
    }

    /// Build a client wrapping a caller-supplied [`Client`].
    ///
    /// Use this when the default transport (a 30 s timeout) is the wrong
    /// policy — for example, when wiring through a corporate proxy, pinning
    /// TLS, or applying a different timeout.
    ///
    /// `subject` must be a VAPID contact URI per RFC 8292 §2.1: either a
    /// `mailto:` URI with a non-empty recipient, or an `https://` URL.
    /// Any other value is rejected at construction so a misconfigured
    /// signer fails fast at startup rather than producing a JWT every push
    /// service silently refuses. Validation is intentionally scheme-shape
    /// only — full RFC 5322 email parsing is out of scope and not what
    /// RFC 8292 requires.
    pub fn with_client(
        http: Client,
        signer: VapidSigner,
        subject: impl Into<String>,
    ) -> Result<Self, WebPushError> {
        let subject = subject.into();
        validate_vapid_subject(&subject)?;
        Ok(Self {
            http,
            signer,
            subject,
            endpoint_policy: EndpointPolicy::default(),
        })
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
                let retry_after =
                    parse_retry_after_secs(resp.headers().get(reqwest::header::RETRY_AFTER));
                let body = read_capped_body(resp, MAX_ERROR_BODY_BYTES).await;
                Err(WebPushError::PushServiceRejected {
                    status,
                    retry_after,
                    body,
                })
            }
        }
    }
}

/// Stream and accumulate up to `cap` bytes of an HTTP response body, then
/// drop the response so the remainder of the body is not buffered. The
/// returned string is UTF-8-lossy — push services may include arbitrary
/// bytes, but the snippet is intended for diagnostic surfacing only.
async fn read_capped_body(mut resp: reqwest::Response, cap: usize) -> String {
    let mut buf: Vec<u8> = Vec::new();
    while buf.len() < cap {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = cap - buf.len();
                let take = remaining.min(chunk.len());
                buf.extend_from_slice(&chunk[..take]);
                if buf.len() >= cap {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    // Drop resp explicitly — closing the connection (or returning it to
    // the pool) prevents the hostile peer from holding the socket open by
    // dribbling more bytes once we've stopped reading.
    drop(resp);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Parse an RFC 7231 `Retry-After` header into a [`Duration`].
///
/// Only the delta-seconds form is parsed; the HTTP-date form (`Wed, 21
/// Oct 2026 07:28:00 GMT`) returns `None`. The vast majority of push
/// services emit delta-seconds, and recognising the date form would
/// require pulling a wider HTTP-date parser without unblocking any
/// real-world retry path. Callers needing date-form support can re-read
/// the original header from a transport hook on their own [`Client`].
fn parse_retry_after_secs(header: Option<&reqwest::header::HeaderValue>) -> Option<Duration> {
    let val = header?.to_str().ok()?;
    let secs: u64 = val.trim().parse().ok()?;
    // Cap the retry hint at 24 hours so a hostile server can't park a
    // worker on a multi-year sleep. Callers can re-fetch the raw header
    // if they have a use case for longer waits.
    const MAX_RETRY_AFTER_SECS: u64 = 24 * 3600;
    Some(Duration::from_secs(secs.min(MAX_RETRY_AFTER_SECS)))
}

/// Validate a VAPID subject claim per RFC 8292 §2.1.
///
/// The subject MUST be a contact URI — either a `mailto:` URI with a
/// non-empty addressee, or an `https://` URL. Anything else is rejected
/// at client construction so a misconfigured signer cannot ship invalid
/// JWTs that push services silently refuse.
///
/// Validation is intentionally shallow:
/// - `mailto:` — accept any non-empty addressee (no RFC 5322 parse;
///   push services accept the same form browsers do).
/// - `https://` — require [`Url::parse`] to succeed and the result to
///   have a host.
fn validate_vapid_subject(subject: &str) -> Result<(), WebPushError> {
    let trimmed = subject.trim();
    if trimmed.is_empty() {
        return Err(WebPushError::Vapid(
            "VAPID subject must be a mailto: or https: contact URI (got empty string)".into(),
        ));
    }
    if let Some(rest) = trimmed.strip_prefix("mailto:") {
        if rest.is_empty() {
            return Err(WebPushError::Vapid(
                "VAPID mailto: subject must have a non-empty addressee".into(),
            ));
        }
        return Ok(());
    }
    if trimmed.starts_with("https://") {
        let url = Url::parse(trimmed).map_err(|e| {
            WebPushError::Vapid(format!("VAPID https subject is not a valid URL: {e}"))
        })?;
        // Reject empty / missing host. `url::Url::host()` returns `None`
        // for hostless schemes; `host_str` can also return `Some("")` for
        // `https:///path`. Both shapes are invalid VAPID subjects.
        match url.host_str() {
            None => {
                return Err(WebPushError::Vapid(
                    "VAPID https subject must have a host component".into(),
                ));
            }
            Some("") => {
                return Err(WebPushError::Vapid(
                    "VAPID https subject must have a non-empty host component".into(),
                ));
            }
            Some(_) => {}
        }
        return Ok(());
    }
    Err(WebPushError::Vapid(format!(
        "VAPID subject must be a mailto: or https: contact URI per RFC 8292 §2.1 (got '{trimmed}')"
    )))
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

    // ---------------------------------------------------------------------
    // VAPID subject validation — RFC 8292 §2.1 requires `mailto:` or
    // `https:` URI for the `sub` claim. Misconfigured subjects fail at
    // client construction so a startup misconfig blows up early instead of
    // producing JWTs every push service silently refuses.
    // ---------------------------------------------------------------------

    #[test]
    fn vapid_subject_accepts_mailto_and_https_urls() {
        for s in [
            "mailto:admin@example.org",
            "mailto:a@b.c",
            "  mailto:ops@example.com  ",
            "https://example.org/contact",
            "https://app.example.org",
        ] {
            validate_vapid_subject(s).unwrap_or_else(|e| panic!("must accept '{s}': {e}"));
        }
    }

    #[test]
    fn vapid_subject_rejects_empty_or_non_uri() {
        for bad in [
            "",
            "   ",
            "admin@example.org",
            "tel:+15555550100",
            "http://example.org/",
            "ftp://example.org/",
        ] {
            let err = validate_vapid_subject(bad).unwrap_err();
            assert!(
                matches!(err, WebPushError::Vapid(_)),
                "expected Vapid error for '{bad}', got: {err:?}"
            );
        }
    }

    #[test]
    fn vapid_subject_rejects_mailto_with_empty_addressee() {
        let err = validate_vapid_subject("mailto:").unwrap_err();
        assert!(
            matches!(err, WebPushError::Vapid(_)),
            "mailto: with no addressee must be rejected, got: {err:?}"
        );
    }

    #[test]
    fn vapid_subject_rejects_malformed_https() {
        // `https://` with no authority is a parser error per RFC 3986 §3.2.
        // The `url` crate surfaces this as "empty host". Either way, it
        // cannot ever be a valid VAPID subject.
        let err = validate_vapid_subject("https://").unwrap_err();
        assert!(
            matches!(err, WebPushError::Vapid(_)),
            "https with no authority must be rejected, got: {err:?}"
        );
    }

    // ---------------------------------------------------------------------
    // Retry-After parsing — only delta-seconds. HTTP-date form is
    // intentionally returned as None (documented behaviour).
    // ---------------------------------------------------------------------

    #[test]
    fn retry_after_parses_delta_seconds() {
        let h = reqwest::header::HeaderValue::from_static("30");
        assert_eq!(
            parse_retry_after_secs(Some(&h)),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn retry_after_trims_whitespace() {
        let h = reqwest::header::HeaderValue::from_static("  120 ");
        assert_eq!(
            parse_retry_after_secs(Some(&h)),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn retry_after_caps_at_24h() {
        // 30 days in seconds — must be clamped to 24h.
        let h = reqwest::header::HeaderValue::from_static("2592000");
        assert_eq!(
            parse_retry_after_secs(Some(&h)),
            Some(Duration::from_secs(24 * 3600))
        );
    }

    #[test]
    fn retry_after_returns_none_for_http_date() {
        // HTTP-date form is intentionally not parsed.
        let h = reqwest::header::HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT");
        assert_eq!(parse_retry_after_secs(Some(&h)), None);
    }

    #[test]
    fn retry_after_returns_none_when_absent() {
        assert_eq!(parse_retry_after_secs(None), None);
    }
}
