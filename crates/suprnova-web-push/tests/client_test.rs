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
        .with_endpoint_policy(EndpointPolicy::AllowAny);

    let err = client
        .send(&sub_for(&server), b"hi", ContentEncoding::Aes128Gcm, 60)
        .await
        .unwrap_err();
    match err {
        WebPushError::PushServiceRejected { status, body } => {
            assert_eq!(status, 429);
            assert!(body.contains("too many"), "got body: {body}");
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
    let client = WebPushClient::new(signer, "mailto:admin@example.org");

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
    let client = WebPushClient::new(signer, "mailto:admin@example.org");

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
    let client = WebPushClient::new(signer, "mailto:admin@example.org");

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
