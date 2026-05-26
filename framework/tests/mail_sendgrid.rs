use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::mail::sendgrid::SendGridMailTransport;
use suprnova::mail::{Address, Mail, Mailable};
use tracing_test::traced_test;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct M {
    // Tera context requires a JSON object; a unit struct serializes to
    // `null` and `Context::from_value` rejects it. Empty named struct
    // → `{}`, which is what Tera accepts.
    _placeholder: (),
}

#[async_trait]
impl Mailable for M {
    fn mailable_name() -> &'static str {
        "M"
    }
    fn subject(&self) -> String {
        "subj".into()
    }
    fn html_template_source(&self) -> Option<String> {
        Some("<p>hi</p>".into())
    }
    fn text_template_source(&self) -> Option<String> {
        Some("hi".into())
    }
    fn from(&self) -> Option<Address> {
        Some("noreply@suprnova.dev".into())
    }
}

#[tokio::test]
#[serial]
async fn sendgrid_emits_v3_mail_send_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v3/mail/send"))
        .and(header("authorization", "Bearer test-api-key"))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;

    let transport = SendGridMailTransport::with_endpoint("test-api-key", server.uri());
    Mail::set_transport(Arc::new(transport));
    Mail::to("alice@example.org")
        .send(M::default())
        .await
        .unwrap();

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(
        body["personalizations"][0]["to"][0]["email"],
        "alice@example.org"
    );
    assert_eq!(body["from"]["email"], "noreply@suprnova.dev");
    assert_eq!(body["subject"], "subj");
    // SendGrid expects content as an array of {type, value}.
    let contents = body["content"].as_array().unwrap();
    assert!(contents.iter().any(|c| c["type"] == "text/plain"));
    assert!(contents.iter().any(|c| c["type"] == "text/html"));

    // RFC 1341 ordering: text/plain MUST precede text/html or v3 returns 400.
    let plain_idx = contents
        .iter()
        .position(|c| c["type"] == "text/plain")
        .unwrap();
    let html_idx = contents
        .iter()
        .position(|c| c["type"] == "text/html")
        .unwrap();
    assert!(
        plain_idx < html_idx,
        "text/plain must precede text/html for SendGrid RFC 1341 compliance: {contents:?}"
    );
}

#[tokio::test]
#[serial]
async fn sendgrid_maps_api_error_to_framework_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "errors": [{
                "message": "The to.email is not a valid email address.",
                "field": "personalizations.0.to.0.email"
            }]
        })))
        .mount(&server)
        .await;

    let transport = SendGridMailTransport::with_endpoint("test-api-key", server.uri());
    Mail::set_transport(Arc::new(transport));
    let err = Mail::to("not-an-email")
        .send(M::default())
        .await
        .unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("SendGrid"), "error mentions provider: {s}");
    assert!(s.contains("400"), "error includes HTTP status: {s}");
    assert!(
        s.contains("The to.email is not a valid email address"),
        "error surfaces upstream body: {s}"
    );
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
struct MWithPdf {
    _placeholder: (),
}

#[async_trait]
impl Mailable for MWithPdf {
    fn mailable_name() -> &'static str {
        "MWithPdf"
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
async fn sendgrid_encodes_attachments_as_base64_with_filename_and_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v3/mail/send"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;

    let transport = SendGridMailTransport::with_endpoint("test-api-key", server.uri());
    Mail::set_transport(Arc::new(transport));
    Mail::to("alice@example.org")
        .send(MWithPdf::default())
        .await
        .unwrap();

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let attachments = body["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["filename"], "invoice.pdf");
    assert_eq!(attachments[0]["type"], "application/pdf");
    assert_eq!(attachments[0]["disposition"], "attachment");

    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(attachments[0]["content"].as_str().unwrap())
        .unwrap();
    assert_eq!(decoded, b"%PDF-1.4\n%test-content");
}

#[tokio::test]
#[serial]
#[traced_test]
async fn sendgrid_warns_when_multiple_reply_to_addresses_are_truncated() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;

    let transport = SendGridMailTransport::with_endpoint("test-api-key", server.uri());
    Mail::set_transport(Arc::new(transport));

    Mail::to("alice@example.org")
        .reply_to("first@reply.example")
        .reply_to("second@reply.example")
        .reply_to("third@reply.example")
        .send(M::default())
        .await
        .unwrap();

    // The wire payload only carries the first reply_to (SendGrid v3 hard-
    // limit); the warn surfaces the kept + dropped addresses so the
    // truncation isn't silent.
    let reqs = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["reply_to"]["email"], "first@reply.example");

    assert!(logs_contain("SendGrid v3 supports only one reply_to"));
    assert!(logs_contain("first@reply.example"));
    assert!(logs_contain("second@reply.example"));
    assert!(logs_contain("third@reply.example"));
}
