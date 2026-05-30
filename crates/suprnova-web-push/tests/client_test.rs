use suprnova_web_push::{
    ContentEncoding, EndpointPolicy, SubscriptionInfo, VapidKey, VapidSigner, WebPushClient,
    WebPushError,
};
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RECEIVER_P256DH: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const RECEIVER_AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

fn sub_for(server: &MockServer) -> SubscriptionInfo {
    SubscriptionInfo {
        endpoint: format!("{}/push", server.uri()),
        keys: suprnova_web_push::client::SubscriptionKeys {
            p256dh: RECEIVER_P256DH.into(),
            auth: RECEIVER_AUTH.into(),
        },
    }
}

#[tokio::test]
async fn client_posts_encrypted_payload_with_vapid_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push"))
        .and(header_exists("authorization"))
        .and(header_exists("crypto-key"))
        .and(header_exists("ttl"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    let key = VapidKey::generate();
    let signer = VapidSigner::new(key);
    // wiremock serves `http://127.0.0.1:<port>`, which the production-default
    // Strict endpoint policy would reject — opt into AllowAny for the mock.
    let client = WebPushClient::new(signer, "mailto:admin@example.org")
        .expect("test subject is a valid mailto: URI")
        .with_endpoint_policy(EndpointPolicy::AllowAny);

    let resp = client
        .send(&sub_for(&server), b"hello", ContentEncoding::Aes128Gcm, 60)
        .await
        .unwrap();
    assert_eq!(resp.status, 201);
}

#[tokio::test]
async fn client_maps_410_to_subscription_gone() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push"))
        .respond_with(ResponseTemplate::new(410))
        .mount(&server)
        .await;

    let signer = VapidSigner::new(VapidKey::generate());
    let client = WebPushClient::new(signer, "mailto:admin@example.org")
        .expect("test subject is a valid mailto: URI")
        .with_endpoint_policy(EndpointPolicy::AllowAny);

    let err = client
        .send(&sub_for(&server), b"hi", ContentEncoding::Aes128Gcm, 60)
        .await
        .unwrap_err();
    assert!(
        matches!(err, WebPushError::SubscriptionGone),
        "got: {err:?}"
    );
}

#[tokio::test]
async fn client_maps_4xx_5xx_to_push_service_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push"))
        .respond_with(ResponseTemplate::new(429).set_body_string("too many requests"))
        .mount(&server)
        .await;

    let signer = VapidSigner::new(VapidKey::generate());
    let client = WebPushClient::new(signer, "mailto:admin@example.org")
        .expect("test subject is a valid mailto: URI")
        .with_endpoint_policy(EndpointPolicy::AllowAny);

    let err = client
        .send(&sub_for(&server), b"hi", ContentEncoding::Aes128Gcm, 60)
        .await
        .unwrap_err();
    match &err {
        WebPushError::PushServiceRejected {
            status,
            retry_after,
            body,
        } => {
            assert_eq!(*status, 429);
            assert!(body.contains("too many"), "got body: {body}");
            // No Retry-After header was set in the response → None.
            assert!(retry_after.is_none(), "got retry_after: {retry_after:?}");
        }
        other => panic!("expected PushServiceRejected, got {other:?}"),
    }
    // 429 is canonically retryable.
    assert!(err.is_retryable(), "429 must be retryable");
}

#[tokio::test]
async fn client_parses_retry_after_delta_seconds_on_429() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "42")
                .set_body_string("slow down"),
        )
        .mount(&server)
        .await;

    let signer = VapidSigner::new(VapidKey::generate());
    let client = WebPushClient::new(signer, "mailto:admin@example.org")
        .expect("valid mailto: subject")
        .with_endpoint_policy(EndpointPolicy::AllowAny);

    let err = client
        .send(&sub_for(&server), b"hi", ContentEncoding::Aes128Gcm, 60)
        .await
        .unwrap_err();
    match err {
        WebPushError::PushServiceRejected {
            status,
            retry_after,
            ..
        } => {
            assert_eq!(status, 429);
            assert_eq!(retry_after, Some(std::time::Duration::from_secs(42)));
        }
        other => panic!("expected PushServiceRejected with retry_after, got {other:?}"),
    }
}

#[tokio::test]
async fn client_is_retryable_classifies_5xx_and_408_and_429() {
    // 503 = retryable; populate a Retry-After to assert is_retryable returns true
    // and the typed Duration round-trips.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("retry-after", "1")
                .set_body_string("maintenance"),
        )
        .mount(&server)
        .await;

    let signer = VapidSigner::new(VapidKey::generate());
    let client = WebPushClient::new(signer, "mailto:admin@example.org")
        .expect("valid mailto: subject")
        .with_endpoint_policy(EndpointPolicy::AllowAny);

    let err = client
        .send(&sub_for(&server), b"hi", ContentEncoding::Aes128Gcm, 60)
        .await
        .unwrap_err();
    assert!(err.is_retryable(), "5xx must be classified retryable");
    assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(1)));
}

#[tokio::test]
async fn client_is_retryable_returns_false_for_4xx_other_than_408_429() {
    // 400 Bad Request — a protocol error, not transient.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push"))
        .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
        .mount(&server)
        .await;

    let signer = VapidSigner::new(VapidKey::generate());
    let client = WebPushClient::new(signer, "mailto:admin@example.org")
        .expect("valid mailto: subject")
        .with_endpoint_policy(EndpointPolicy::AllowAny);

    let err = client
        .send(&sub_for(&server), b"hi", ContentEncoding::Aes128Gcm, 60)
        .await
        .unwrap_err();
    assert!(!err.is_retryable(), "400 must be classified non-retryable");
}

#[tokio::test]
async fn client_caps_rejection_body_at_a_few_kib() {
    // A hostile push service streams a giant rejection body. The client must
    // accumulate at most a few KiB and drop the rest — the returned body
    // length must be bounded regardless of how large the response is.
    let server = MockServer::start().await;
    // 1 MiB of 'A' bytes — far larger than the 8 KiB internal cap.
    let huge_body = "A".repeat(1024 * 1024);
    Mock::given(method("POST"))
        .and(path("/push"))
        .respond_with(ResponseTemplate::new(500).set_body_string(huge_body.clone()))
        .mount(&server)
        .await;

    let signer = VapidSigner::new(VapidKey::generate());
    let client = WebPushClient::new(signer, "mailto:admin@example.org")
        .expect("valid mailto: subject")
        .with_endpoint_policy(EndpointPolicy::AllowAny);

    let err = client
        .send(&sub_for(&server), b"hi", ContentEncoding::Aes128Gcm, 60)
        .await
        .unwrap_err();
    match err {
        WebPushError::PushServiceRejected { status, body, .. } => {
            assert_eq!(status, 500);
            // Body must be bounded — never anywhere close to the 1 MiB sent.
            assert!(
                body.len() <= 16 * 1024,
                "rejection body must be capped, got {} bytes",
                body.len()
            );
            // And the bytes we did capture are the start of the response,
            // not random middle/end — first chars must be the 'A's we sent.
            assert!(body.starts_with('A'), "captured prefix wrong");
        }
        other => panic!("expected PushServiceRejected, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// SSRF guard: production-default Strict policy rejects untrusted endpoint
// shapes before the HTTP POST happens. AllowAny is for tests only.
// ---------------------------------------------------------------------------

fn sub_with_endpoint(endpoint: &str) -> SubscriptionInfo {
    SubscriptionInfo {
        endpoint: endpoint.into(),
        keys: suprnova_web_push::client::SubscriptionKeys {
            p256dh: RECEIVER_P256DH.into(),
            auth: RECEIVER_AUTH.into(),
        },
    }
}

#[tokio::test]
async fn strict_default_rejects_http_endpoint() {
    let signer = VapidSigner::new(VapidKey::generate());
    let client =
        WebPushClient::new(signer, "mailto:admin@example.org").expect("valid mailto: subject");

    let err = client
        .send(
            &sub_with_endpoint("http://fcm.googleapis.com/push/abc"),
            b"hi",
            ContentEncoding::Aes128Gcm,
            60,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("https"),
        "Strict policy must reject http://, got: {err}"
    );
}

#[tokio::test]
async fn strict_default_rejects_ip_literal_endpoint() {
    let signer = VapidSigner::new(VapidKey::generate());
    let client =
        WebPushClient::new(signer, "mailto:admin@example.org").expect("valid mailto: subject");

    let err = client
        .send(
            &sub_with_endpoint("https://169.254.169.254/latest/meta-data"),
            b"hi",
            ContentEncoding::Aes128Gcm,
            60,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("IP literal"),
        "Strict policy must reject IP-literal hosts, got: {err}"
    );
}

#[tokio::test]
async fn strict_default_rejects_metadata_host() {
    let signer = VapidSigner::new(VapidKey::generate());
    let client =
        WebPushClient::new(signer, "mailto:admin@example.org").expect("valid mailto: subject");

    let err = client
        .send(
            &sub_with_endpoint("https://metadata.google.internal/computeMetadata/v1/"),
            b"hi",
            ContentEncoding::Aes128Gcm,
            60,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("not a valid push service host"),
        "Strict policy must reject cloud-metadata hostnames, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// VAPID subject — invalid subjects must fail at construction so a startup
// misconfig blows up early instead of producing JWTs every push service
// silently refuses (RFC 8292 §2.1).
// ---------------------------------------------------------------------------

#[test]
fn new_rejects_non_uri_subject() {
    let signer = VapidSigner::new(VapidKey::generate());
    let err = WebPushClient::new(signer, "admin@example.org").unwrap_err();
    assert!(
        matches!(err, WebPushError::Vapid(_)),
        "bare email must be rejected, got: {err:?}"
    );
}

#[test]
fn new_rejects_http_subject() {
    let signer = VapidSigner::new(VapidKey::generate());
    // RFC 8292 requires `mailto:` or `https:` — http:// is not acceptable.
    let err = WebPushClient::new(signer, "http://example.org").unwrap_err();
    assert!(
        matches!(err, WebPushError::Vapid(_)),
        "http:// must be rejected, got: {err:?}"
    );
}

#[test]
fn new_accepts_mailto_subject() {
    let signer = VapidSigner::new(VapidKey::generate());
    WebPushClient::new(signer, "mailto:admin@example.org").expect("mailto: must succeed");
}

#[test]
fn new_accepts_https_subject() {
    let signer = VapidSigner::new(VapidKey::generate());
    WebPushClient::new(signer, "https://example.org/contact").expect("https:// must succeed");
}
