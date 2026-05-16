//! Integration tests for streaming multipart uploads.
//!
//! Task 4 lands the synthetic-`Request` helpers and the smoke test
//! below; Task 5 ships the real `parse_multipart_streaming` parser and
//! un-ignores the smoke test.

mod common;

use common::{build_multipart_body, request_from_multipart};
use suprnova::http::upload::parse_multipart_streaming;

#[tokio::test]
#[ignore = "parse_multipart_streaming lands in Task 5 Step 2"]
async fn multipart_parses_two_fields_via_helper() {
    let body = build_multipart_body(
        "test",
        &[
            ("avatar", Some("a.bin"), b"image-bytes"),
            ("caption", None, b"hello"),
        ],
    );
    let req = request_from_multipart("test", body).await;
    let payload = parse_multipart_streaming(req, |_, _| Ok(())).await.unwrap();
    // Check both field names landed in the order-preserving Vec.
    let names: Vec<&str> = payload.fields.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"avatar"));
    assert!(names.contains(&"caption"));
}
