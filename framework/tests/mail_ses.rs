use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::mail::ses::SesMailTransport;
use suprnova::mail::{Address, Mail, Mailable};
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct M {
    // Tera context requires a JSON object; a unit struct serializes to `null`
    // and `Context::from_value` rejects that. Empty named struct → `{}`.
    _placeholder: (),
}

#[async_trait]
impl Mailable for M {
    fn mailable_name() -> &'static str {
        "M"
    }
    fn subject(&self) -> String {
        "s".into()
    }
    fn text_template_source(&self) -> Option<String> {
        Some("b".into())
    }
    fn from(&self) -> Option<Address> {
        Some("noreply@suprnova.dev".into())
    }
}

#[tokio::test]
#[serial]
async fn ses_emits_sigv4_signed_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/email/outbound-emails"))
        .and(header_exists("authorization")) // sigv4 puts the sig here
        .and(header_exists("x-amz-date")) // sigv4 timestamp
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MessageId": "0000018a-stub"
        })))
        .mount(&server)
        .await;

    let transport =
        SesMailTransport::with_endpoint("AKIATEST", "secret", "us-east-1", server.uri());
    Mail::set_transport(Arc::new(transport));
    Mail::to("alice@example.org").send(M::default()).await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let auth = reqs[0]
        .headers
        .get("authorization")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(auth.starts_with("AWS4-HMAC-SHA256"), "got: {auth}");
}

#[tokio::test]
#[serial]
async fn ses_maps_4xx_to_framework_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "__type": "MessageRejected",
            "message": "Email address is not verified."
        })))
        .mount(&server)
        .await;

    let transport =
        SesMailTransport::with_endpoint("AKIATEST", "secret", "us-east-1", server.uri());
    Mail::set_transport(Arc::new(transport));
    let err = Mail::to("u@unverified.example")
        .send(M::default())
        .await
        .unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("SES") || s.contains("400"), "got: {s}");
}
