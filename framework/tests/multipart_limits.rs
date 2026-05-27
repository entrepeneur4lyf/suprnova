//! Streaming multipart DoS limits: the total part-count ceiling,
//! `Content-Length` pre-rejection, per-field `max_count` enforced during
//! streaming, and oversized-text-field rejection at the spill threshold.
//!
//! Each test asserts the *mechanism*, not just the status code: the
//! part-count and per-field tests count validator callbacks (which fire
//! only for parts that reach `collect_part`) to prove the parser bails out
//! *before reading* the offending part; the Content-Length test sends a
//! body smaller than the cap so a 413 can only come from the header
//! pre-check; the text-spill test contrasts a text field (rejected at the
//! threshold) against a file field of the same size (spilled to disk).

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};

use common::{build_multipart_body, request_from_multipart, request_with_declared_length};
use suprnova::http::upload::{
    MultipartLimits, parse_multipart_streaming_with_limits, upload_tempfiles_spilled_total,
};

const CT: &str = "multipart/form-data; boundary=test";

// ── High: the total part-count ceiling bails out mid-stream ──

#[tokio::test]
async fn part_count_ceiling_bails_out_before_reading_every_part() {
    // 12 tiny parts, ceiling of 5. A fix that only checks the count after
    // parsing would read all 12; the streaming guard rejects the 6th part
    // before it is read. The validator fires once per chunk inside
    // `collect_part`, which only runs for accepted parts — so it must
    // observe at most `max_parts` parts.
    let parts: Vec<(&str, Option<&str>, &[u8])> =
        (0..12).map(|_| ("p", None, b"x" as &[u8])).collect();
    let body = build_multipart_body("test", &parts);
    let req = request_from_multipart("test", body).await;

    let seen = AtomicUsize::new(0);
    let err = parse_multipart_streaming_with_limits(
        req,
        MultipartLimits {
            max_body_bytes: 64 * 1024,
            max_parts: 5,
            spill_threshold: 64 * 1024,
            per_field_max_counts: &[],
        },
        |_name, _sniff, _size| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    )
    .await
    .err()
    .expect("a 12-part body must be rejected when the ceiling is 5");

    assert_eq!(err.status_code(), 413, "part-count overflow is a 413");
    assert!(
        err.to_string().contains("exceeds 5 parts"),
        "message names the part ceiling; got: {err}"
    );
    assert!(
        seen.load(Ordering::SeqCst) <= 5,
        "parser must stop reading parts at the ceiling: saw {} part-chunks, expected <= 5",
        seen.load(Ordering::SeqCst)
    );
}

// ── Medium: Content-Length pre-rejection without reading the body ──

#[tokio::test]
async fn declared_oversized_content_length_is_pre_rejected() {
    // The body is a few bytes — well under the 1 MiB cap — but the request
    // DECLARES a 50 MiB Content-Length. Because the actual body is under
    // the cap, the progressive per-chunk cap could never fire on it, so a
    // 413 here proves the header pre-check rejected the request before the
    // body was streamed.
    let body = build_multipart_body("test", &[("f", Some("a.bin"), b"tiny")]);
    let declared = 50 * 1024 * 1024u64;
    let req = request_with_declared_length("/upload", CT, declared, &body).await;

    let err = parse_multipart_streaming_with_limits(
        req,
        MultipartLimits {
            max_body_bytes: 1024 * 1024,
            max_parts: 1000,
            spill_threshold: 64 * 1024,
            per_field_max_counts: &[],
        },
        |_n, _s, _z| Ok(()),
    )
    .await
    .err()
    .expect("a declared Content-Length above the cap must be rejected");

    assert_eq!(err.status_code(), 413);
    assert!(
        err.to_string().contains("exceeds 1048576 bytes"),
        "pre-reject names the cap; got: {err}"
    );
}

// ── High: per-field `max_count` enforced during streaming ──

#[tokio::test]
async fn per_field_max_count_rejects_before_reading_the_extra_part() {
    let body = build_multipart_body(
        "test",
        &[
            ("files", Some("1.bin"), b"a"),
            ("files", Some("2.bin"), b"b"),
            ("files", Some("3.bin"), b"c"), // the (cap+1)-th — must be rejected unread
        ],
    );
    let req = request_from_multipart("test", body).await;

    let seen = AtomicUsize::new(0);
    let err = parse_multipart_streaming_with_limits(
        req,
        MultipartLimits {
            max_body_bytes: 64 * 1024,
            max_parts: 1000,
            spill_threshold: 64 * 1024,
            per_field_max_counts: &[("files", 2)],
        },
        |_n, _s, _z| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    )
    .await
    .err()
    .expect("a 3rd 'files' part must be rejected when max_count is 2");

    assert_eq!(err.status_code(), 422);
    assert!(
        err.to_string().contains("exceeds max_count 2"),
        "got: {err}"
    );
    assert!(
        seen.load(Ordering::SeqCst) <= 2,
        "the 3rd part must be rejected before it is read: saw {}",
        seen.load(Ordering::SeqCst)
    );
}

// ── Medium: oversized text field rejected at the spill threshold, no temp file ──

#[tokio::test]
async fn oversized_text_field_rejected_at_threshold_without_spilling() {
    let big = vec![b'a'; 4096];

    // A text part (no filename) that crosses the 64-byte spill threshold is
    // rejected at the threshold rather than spilled to disk.
    let text_body = build_multipart_body("test", &[("bio", None, &big)]);
    let text_req = request_from_multipart("test", text_body).await;
    let text_err = parse_multipart_streaming_with_limits(
        text_req,
        MultipartLimits {
            max_body_bytes: 1024 * 1024,
            max_parts: 1000,
            spill_threshold: 64,
            per_field_max_counts: &[],
        },
        |_n, _s, _z| Ok(()),
    )
    .await
    .err()
    .expect("an oversized text field must be rejected");
    assert_eq!(text_err.status_code(), 400);
    assert!(
        text_err.to_string().contains("in-memory limit"),
        "text rejection names the in-memory limit; got: {text_err}"
    );

    // Positive control: a file part of the SAME size spills to disk and is
    // accepted, and the global spill counter advances — confirming the
    // disk path that the text field deliberately avoids. `>= before + 1`
    // is robust to other upload tests spilling concurrently.
    let before = upload_tempfiles_spilled_total();
    let file_body = build_multipart_body("test", &[("doc", Some("d.bin"), &big)]);
    let file_req = request_from_multipart("test", file_body).await;
    let payload = parse_multipart_streaming_with_limits(
        file_req,
        MultipartLimits {
            max_body_bytes: 1024 * 1024,
            max_parts: 1000,
            spill_threshold: 64,
            per_field_max_counts: &[],
        },
        |_n, _s, _z| Ok(()),
    )
    .await
    .expect("a file part above the spill threshold spills to disk and is accepted");
    assert_eq!(payload.fields.len(), 1);
    assert!(
        upload_tempfiles_spilled_total() > before,
        "the file part must have spilled to a temp file"
    );
}
