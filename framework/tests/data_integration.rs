//! End-to-end integration tests for `#[derive(Data)]` used as both inbound
//! `FormRequest` and outbound Inertia response — the "one struct, both ends"
//! pattern.
//!
//! Test patterns:
//! - Tests A & D: TCP-listener + hyper client (mirrors `data_form_request.rs`)
//! - Test B:      Pure serde, no HTTP
//! - Test C:      MockReq in-memory (mirrors `data_partial_data_composition.rs`)

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::data::Field;
use suprnova::error::FrameworkError;
use suprnova::{FormRequest, HttpResponse, InertiaRequestExt, InertiaResponse, Request};

// ---------------------------------------------------------------------------
// Shared DTO under test
// ---------------------------------------------------------------------------

#[derive(Debug, suprnova::Data, validator::Validate)]
pub struct TestArticleDtoT12 {
    pub id: i64,

    #[validate(length(min = 1, max = 255))]
    pub title: String,

    #[data(input_only)]
    pub draft_body: String,

    #[data(output_only)]
    pub published_html: String,

    pub summary: Field<String>,
}

// ---------------------------------------------------------------------------
// TCP-listener helpers (pattern from data_form_request.rs)
// ---------------------------------------------------------------------------

async fn spawn_and_capture()
-> (
    SocketAddr,
    Arc<Mutex<Option<Result<TestArticleDtoT12, FrameworkError>>>>,
) {
    let captured: Arc<Mutex<Option<Result<TestArticleDtoT12, FrameworkError>>>> =
        Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let captured_server = captured.clone();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let io = TokioIo::new(stream);
            let captured_svc = captured_server.clone();
            let svc = service_fn(
                move |hyper_req: hyper::Request<hyper::body::Incoming>| {
                    let captured_inner = captured_svc.clone();
                    async move {
                        let req = Request::new(hyper_req);
                        let result = TestArticleDtoT12::extract(req).await;
                        *captured_inner.lock().unwrap() = Some(result);
                        Ok::<_, Infallible>(HttpResponse::text("ok").into_hyper())
                    }
                },
            );
            let _ = http1::Builder::new().serve_connection(io, svc).await;
        }
    });

    (addr, captured)
}

async fn post_json(addr: SocketAddr, body: serde_json::Value) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let req = hyper::Request::builder()
        .method("POST")
        .uri("http://localhost/articles")
        .header("content-type", "application/json")
        .header("content-length", body_bytes.len())
        .body(Full::new(Bytes::from(body_bytes)))
        .unwrap();

    let _ = sender.send_request(req).await;
}

// ---------------------------------------------------------------------------
// MockReq helper (pattern from data_partial_data_composition.rs)
// ---------------------------------------------------------------------------

struct MockReq {
    path: String,
    headers: HashMap<String, String>,
}

impl MockReq {
    fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            headers: HashMap::new(),
        }
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name.to_string(), value.to_string());
        self
    }

    fn inertia(self) -> Self {
        self.with_header("X-Inertia", "true")
    }
}

impl InertiaRequestExt for MockReq {
    fn path(&self) -> &str {
        &self.path
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|s| s.as_str())
    }
}

// Body reader helper
async fn body_to_string(
    body: http_body_util::combinators::BoxBody<Bytes, Infallible>,
) -> String {
    use http_body_util::BodyExt;
    let bytes = body.collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

// ---------------------------------------------------------------------------
// Test A: inbound path validates and constructs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inbound_path_validates_and_constructs() {
    let (addr, captured) = spawn_and_capture().await;
    post_json(
        addr,
        serde_json::json!({
            "id": 1,
            "title": "Hello world",
            "draft_body": "# Hello",
            "summary": "A greeting"
        }),
    )
    .await;

    tokio::task::yield_now().await;

    let result = captured
        .lock()
        .unwrap()
        .take()
        .expect("server did not process request");

    let article = result.expect("expected Ok, got Err");

    assert_eq!(article.title, "Hello world");
    assert_eq!(article.draft_body, "# Hello");
    assert!(
        matches!(&article.summary, Field::Value(s) if s == "A greeting"),
        "expected Field::Value(\"A greeting\"), got {:?}",
        article.summary
    );
    // output_only field is defaulted (empty string) on deserialize
    assert_eq!(
        article.published_html, "",
        "output_only field should default to empty string on inbound deserialize"
    );
}

// ---------------------------------------------------------------------------
// Test B: outbound path strips input_only and includes output_only
// ---------------------------------------------------------------------------

#[test]
fn outbound_path_strips_input_only_and_includes_output_only() {
    let article = TestArticleDtoT12 {
        id: 1,
        title: "Hello".into(),
        draft_body: "# Hello".into(),
        published_html: "<h1>Hello</h1>".into(),
        summary: Field::Absent,
    };

    let j = serde_json::to_value(&article).unwrap();

    assert_eq!(j["id"], 1, "id should be present");
    assert_eq!(j["title"], "Hello", "title should be present");

    // input_only field must be stripped from serialized output
    assert!(
        j.get("draft_body").is_none(),
        "draft_body (input_only) should be absent from serialized output, got: {:?}",
        j.get("draft_body")
    );

    // output_only field must be present in serialized output
    assert_eq!(
        j["published_html"], "<h1>Hello</h1>",
        "published_html (output_only) should be present in serialized output"
    );

    // Field::Absent serializes as null (serialize_none()) — the macro's custom
    // Serialize impl does not honor skip_serializing_if on field attrs, so the
    // key IS present with a null value.
    assert!(
        j["summary"].is_null(),
        "summary (Field::Absent) should serialize as null, got: {:?}",
        j.get("summary")
    );
}

// ---------------------------------------------------------------------------
// Test C: outbound via Inertia response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn outbound_via_inertia_response() {
    let article = TestArticleDtoT12 {
        id: 1,
        title: "Hello".into(),
        draft_body: "# Hello".into(),
        published_html: "<h1>Hello</h1>".into(),
        summary: Field::Value("Greeting".into()),
    };

    let req = MockReq::new("/articles/1").inertia();

    let resp = InertiaResponse::new("Article/Show")
        .with("article", serde_json::to_value(&article).unwrap())
        .resolve(&req)
        .await
        .unwrap();

    let body = body_to_string(resp.into_hyper().into_body()).await;
    let page: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(
        page["props"]["article"]["title"], "Hello",
        "title should be present in Inertia response"
    );
    assert!(
        page["props"]["article"].get("draft_body").is_none(),
        "draft_body (input_only) should be absent after serialization, got: {:?}",
        page["props"]["article"].get("draft_body")
    );
    assert_eq!(
        page["props"]["article"]["published_html"], "<h1>Hello</h1>",
        "published_html (output_only) should be present in Inertia response"
    );
    // Field::Value("Greeting") serializes as the inner value
    assert_eq!(
        page["props"]["article"]["summary"], "Greeting",
        "summary Field::Value should serialize as inner string value"
    );
}

// ---------------------------------------------------------------------------
// Test D: inbound rejects output_only in payload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inbound_rejects_output_only_in_payload() {
    let (addr, captured) = spawn_and_capture().await;
    post_json(
        addr,
        serde_json::json!({
            "id": 1,
            "title": "Hello",
            "draft_body": "# Hello",
            "published_html": "<h1>injected</h1>"
        }),
    )
    .await;

    tokio::task::yield_now().await;

    let result = captured
        .lock()
        .unwrap()
        .take()
        .expect("server did not process request");

    let err = result.expect_err("expected Err for output_only field in payload");
    assert_eq!(
        err.status_code(),
        422,
        "expected 422 Unprocessable Entity when output_only field is provided in input, got {}",
        err.status_code()
    );
}
