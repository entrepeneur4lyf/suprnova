use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::mail::resend::ResendMailTransport;
use suprnova::mail::{Address, Mail, Mailable};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct M {
    // Tera context requires a JSON object; an empty named struct serializes
    // to `{}`, while a unit struct would serialize to `null` and Tera rejects.
    _placeholder: (),
}

#[async_trait]
impl Mailable for M {
    fn mailable_name() -> &'static str { "M" }
    fn subject(&self) -> String { "subj".into() }
    fn html_template_source(&self) -> Option<String> { Some("<p>hi</p>".into()) }
    fn from(&self) -> Option<Address> { Some("noreply@suprnova.dev".into()) }
}

#[tokio::test]
#[serial]
async fn resend_emits_v1_emails_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/emails"))
        .and(header("authorization", "Bearer test-key"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "msg_abc"
        })))
        .mount(&server)
        .await;

    let transport = ResendMailTransport::with_endpoint("test-key", server.uri());
    Mail::set_transport(Arc::new(transport));
    Mail::to("alice@example.org").send(M::default()).await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["from"], "noreply@suprnova.dev");
    assert_eq!(body["to"][0], "alice@example.org");
    assert_eq!(body["subject"], "subj");
    assert_eq!(body["html"], "<p>hi</p>");
}

#[tokio::test]
#[serial]
async fn resend_maps_api_error_to_framework_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
            "name": "validation_error",
            "message": "Invalid `to` field"
        })))
        .mount(&server)
        .await;

    let transport = ResendMailTransport::with_endpoint("test-key", server.uri());
    Mail::set_transport(Arc::new(transport));
    let err = Mail::to("bad@example.org").send(M::default()).await.unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("Resend"), "error mentions provider: {s}");
    assert!(s.contains("422"), "error includes HTTP status: {s}");
    assert!(s.contains("Invalid `to` field"), "error surfaces upstream body: {s}");
}

// Attachment test: Resend's JSON shape uses
// `attachments: [{filename, content (base64), content_type}]`.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct WithPdf {
    _placeholder: (),
}

#[async_trait]
impl Mailable for WithPdf {
    fn mailable_name() -> &'static str { "WithPdf" }
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
async fn resend_encodes_attachments_as_base64_with_filename_and_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/emails"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": "x" })))
        .mount(&server)
        .await;

    let transport = ResendMailTransport::with_endpoint("test-key", server.uri());
    Mail::set_transport(Arc::new(transport));
    Mail::to("alice@example.org").send(WithPdf::default()).await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let attachments = body["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["filename"], "invoice.pdf");
    assert_eq!(attachments[0]["content_type"], "application/pdf");

    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(attachments[0]["content"].as_str().unwrap())
        .unwrap();
    assert_eq!(decoded, b"%PDF-1.4\n%test-content");
}
