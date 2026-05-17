use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::mail::mailgun::MailgunMailTransport;
use suprnova::mail::{Address, Mail, Mailable};
use wiremock::matchers::{header, method, path};
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
async fn mailgun_emits_form_encoded_request_with_basic_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v3/example.com/messages"))
        // base64("api:test-key") → "YXBpOnRlc3Qta2V5"
        .and(header("authorization", "Basic YXBpOnRlc3Qta2V5"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "<2026.0517.test@mg.example.com>",
            "message": "Queued. Thank you."
        })))
        .mount(&server)
        .await;

    let transport = MailgunMailTransport::with_endpoint("test-key", "example.com", server.uri());
    Mail::set_transport(Arc::new(transport));
    Mail::to("alice@example.org").send(M::default()).await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    let body = std::str::from_utf8(&reqs[0].body).unwrap();
    assert!(body.contains("from=noreply%40suprnova.dev"), "got: {body}");
    assert!(body.contains("to=alice%40example.org"), "got: {body}");
    assert!(body.contains("subject=s"), "got: {body}");
    assert!(body.contains("text=b"), "got: {body}");
}

#[tokio::test]
#[serial]
async fn mailgun_maps_4xx_to_framework_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "message": "'to' parameter is not a valid address. please check documentation"
        })))
        .mount(&server)
        .await;

    let transport = MailgunMailTransport::with_endpoint("test-key", "example.com", server.uri());
    Mail::set_transport(Arc::new(transport));
    let err = Mail::to("not-an-email")
        .send(M::default())
        .await
        .unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("Mailgun"), "error mentions provider: {s}");
    assert!(s.contains("400"), "error includes HTTP status: {s}");
    assert!(
        s.contains("'to' parameter is not a valid address"),
        "error surfaces upstream body: {s}"
    );
}

// Attachments must switch to multipart/form-data; Mailgun's form-encoded API
// does not accept file uploads, so dropping silently would be a data-loss bug.
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
async fn mailgun_uses_multipart_form_data_when_attachments_present() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v3/example.com/messages"))
        .and(header("authorization", "Basic YXBpOnRlc3Qta2V5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "<2026.0517.attach@mg.example.com>",
            "message": "Queued. Thank you."
        })))
        .mount(&server)
        .await;

    let transport = MailgunMailTransport::with_endpoint("test-key", "example.com", server.uri());
    Mail::set_transport(Arc::new(transport));
    Mail::to("alice@example.org")
        .send(MWithPdf::default())
        .await
        .unwrap();

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);

    // reqwest's multipart::Form generates its own boundary, so match the
    // header prefix rather than a fixed string.
    let content_type = reqs[0]
        .headers
        .get("content-type")
        .expect("content-type header present")
        .to_str()
        .expect("content-type is ASCII");
    assert!(
        content_type.starts_with("multipart/form-data; boundary="),
        "content-type must announce multipart with boundary: {content_type}"
    );

    let body = String::from_utf8_lossy(&reqs[0].body);

    // Form fields ride the multipart envelope.
    assert!(
        body.contains("Content-Disposition: form-data; name=\"from\""),
        "from field present: {body}"
    );
    assert!(
        body.contains("noreply@suprnova.dev"),
        "from value present: {body}"
    );
    assert!(
        body.contains("Content-Disposition: form-data; name=\"to\""),
        "to field present: {body}"
    );
    assert!(
        body.contains("alice@example.org"),
        "to value present: {body}"
    );
    assert!(
        body.contains("Content-Disposition: form-data; name=\"subject\""),
        "subject field present: {body}"
    );
    assert!(body.contains("invoice"), "subject value present: {body}");
    assert!(
        body.contains("Content-Disposition: form-data; name=\"text\""),
        "text field present: {body}"
    );
    assert!(
        body.contains("see attached"),
        "text body value present: {body}"
    );

    // The attachment part must use the literal `attachment` field name with
    // the original filename and content-type, and the raw bytes must round-
    // trip unchanged.
    assert!(
        body.contains(
            "Content-Disposition: form-data; name=\"attachment\"; filename=\"invoice.pdf\""
        ),
        "attachment disposition present: {body}"
    );
    assert!(
        body.contains("Content-Type: application/pdf"),
        "attachment content-type present: {body}"
    );
    assert!(
        body.contains("%PDF-1.4\n%test-content"),
        "attachment raw bytes round-trip: {body}"
    );
}

#[tokio::test]
#[serial]
async fn mailgun_routes_to_eu_region_endpoint() {
    let server = MockServer::start().await;
    // Pin the path so a wrong endpoint (e.g. accidental US default) would
    // 404 against the mock instead of silently passing.
    Mock::given(method("POST"))
        .and(path("/v3/eu-tenant.example/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "<eu-region@mg.example.com>",
            "message": "Queued. Thank you."
        })))
        .mount(&server)
        .await;

    // The mock server's URI stands in for `https://api.eu.mailgun.net` —
    // what matters for routing is that the constructor preserved the host
    // and built `<host>/v3/<domain>/messages` exactly.
    let transport =
        MailgunMailTransport::with_endpoint("test-key", "eu-tenant.example", server.uri());
    Mail::set_transport(Arc::new(transport));
    Mail::to("bob@example.org").send(M::default()).await.unwrap();

    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].url.path(), "/v3/eu-tenant.example/messages");
}
