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
    assert!(s.contains("SES"), "error mentions provider: {s}");
    assert!(s.contains("400"), "error includes HTTP status: {s}");
    assert!(s.contains("Email address is not verified"), "error surfaces upstream body: {s}");
}

// Attachments must ride the Raw MIME path — SES `Content.Simple` has no
// attachment support, so dropping silently is a data-loss bug.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct MWithPdf {
    _placeholder: (),
}

#[async_trait]
impl Mailable for MWithPdf {
    fn mailable_name() -> &'static str { "MWithPdf" }
    fn subject(&self) -> String { "invoice".into() }
    fn text_template_source(&self) -> Option<String> { Some("see attached".into()) }
    fn from(&self) -> Option<Address> { Some("noreply@suprnova.dev".into()) }
    fn attachments(&self) -> Vec<suprnova::mail::Attachment> {
        vec![suprnova::mail::Attachment {
            filename: "invoice.pdf".into(),
            content: b"%PDF-1.4\n%test-content".to_vec(),
            content_type: "application/pdf".into(),
        }]
    }
}

#[tokio::test]
#[serial]
async fn ses_uses_raw_mime_when_attachments_present() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v2/email/outbound-emails"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MessageId": "raw-stub"
        })))
        .mount(&server)
        .await;

    let transport =
        SesMailTransport::with_endpoint("AKIATEST", "secret", "us-east-1", server.uri());
    Mail::set_transport(Arc::new(transport));
    Mail::to("alice@example.org")
        .send(MWithPdf::default())
        .await
        .unwrap();

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();

    // Must use the Raw variant when attachments are present — Simple has
    // no attachment support and would silently drop them.
    assert!(body["Content"]["Simple"].is_null(), "Simple variant must be absent: {body}");
    let raw_b64 = body["Content"]["Raw"]["Data"]
        .as_str()
        .expect("Content.Raw.Data is a string");

    use base64::Engine;
    let mime = base64::engine::general_purpose::STANDARD
        .decode(raw_b64)
        .expect("Raw.Data is valid base64");
    let mime_str = String::from_utf8_lossy(&mime);

    assert!(
        mime_str.contains("Content-Disposition: attachment"),
        "MIME has attachment disposition: {mime_str}"
    );
    assert!(mime_str.contains("invoice.pdf"), "MIME contains filename: {mime_str}");
    assert!(mime_str.contains("application/pdf"), "MIME contains content-type: {mime_str}");
    // Body and subject must still ride the MIME envelope too.
    assert!(mime_str.contains("invoice"), "MIME contains subject: {mime_str}");
    assert!(
        mime_str.contains("see attached"),
        "MIME contains text body: {mime_str}"
    );
}
