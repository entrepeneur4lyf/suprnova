use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::mail::postmark::PostmarkMailTransport;
use suprnova::mail::{Address, Mail, Mailable};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct Bing {
    // Tera context requires a JSON object; an empty named struct serializes
    // to `{}`, while a unit struct would serialize to `null` and Tera rejects.
    _placeholder: (),
}

#[async_trait]
impl Mailable for Bing {
    fn mailable_name() -> &'static str {
        "Bing"
    }
    fn subject(&self) -> String {
        "subj".into()
    }
    fn text_template_source(&self) -> Option<String> {
        Some("body".into())
    }
    fn from(&self) -> Option<Address> {
        Some(("Suprnova", "noreply@suprnova.dev").into())
    }
}

#[tokio::test]
#[serial]
async fn postmark_emits_correct_http_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/email"))
        .and(header("x-postmark-server-token", "test-token"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "MessageID": "abc-123",
            "SubmittedAt": "2026-05-16T12:00:00Z",
            "ErrorCode": 0,
            "Message": "OK"
        })))
        .mount(&server)
        .await;

    let transport = PostmarkMailTransport::with_endpoint("test-token", server.uri());
    let _ = Mail::set_transport(Arc::new(transport));
    Mail::to("alice@example.org")
        .send(Bing::default())
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["From"], "Suprnova <noreply@suprnova.dev>");
    assert_eq!(body["To"], "alice@example.org");
    assert_eq!(body["Subject"], "subj");
    assert_eq!(body["TextBody"], "body");
}

#[tokio::test]
#[serial]
async fn postmark_maps_api_error_to_framework_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
            "ErrorCode": 405,
            "Message": "Inactive recipient"
        })))
        .mount(&server)
        .await;

    let transport = PostmarkMailTransport::with_endpoint("test-token", server.uri());
    let _ = Mail::set_transport(Arc::new(transport));
    let err = Mail::to("blocked@example.org")
        .send(Bing::default())
        .await
        .unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("Postmark"), "error mentions provider: {s}");
    assert!(s.contains("422"), "error includes HTTP status: {s}");
    assert!(
        s.contains("Inactive recipient"),
        "error surfaces upstream body: {s}"
    );
}

// Attachment test: shared HTTP-provider attachment shape (SES/SendGrid/Resend reuse it).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct WithPdf {
    _placeholder: (),
}

#[async_trait]
impl Mailable for WithPdf {
    fn mailable_name() -> &'static str {
        "WithPdf"
    }
    fn subject(&self) -> String {
        "invoice".into()
    }
    fn text_template_source(&self) -> Option<String> {
        Some("see attached".into())
    }
    fn from(&self) -> Option<Address> {
        Some("noreply@suprnova.dev".into())
    }
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
async fn postmark_encodes_attachments_as_base64_with_filename_and_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/email"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "MessageID": "x" })),
        )
        .mount(&server)
        .await;

    let transport = PostmarkMailTransport::with_endpoint("test-token", server.uri());
    let _ = Mail::set_transport(Arc::new(transport));
    Mail::to("alice@example.org")
        .send(WithPdf::default())
        .await
        .unwrap();

    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let attachments = body["Attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["Name"], "invoice.pdf");
    assert_eq!(attachments[0]["ContentType"], "application/pdf");

    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(attachments[0]["Content"].as_str().unwrap())
        .unwrap();
    assert_eq!(decoded, b"%PDF-1.4\n%test-content");
}
