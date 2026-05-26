//! Verify the generic JSON/form/raw body cap (codex finding #5).
//!
//! Mirrors the multipart cap test surface in `uploads.rs`:
//! - Compile-time default rejects oversized bodies.
//! - Content-Length is pre-checked before any read.
//! - Process-global override changes the cap at boot.
//! - Per-FormRequest override beats the global.
//! - Progressive cap catches lying / absent Content-Length during read.
//! - All overflow paths return HTTP 413 via `FrameworkError::Domain`.

mod common;

use common::{request_with_body, request_with_chunked_body, request_with_declared_length};
use serde::Deserialize;
use std::sync::Mutex;
use suprnova::FormRequest;
use validator::Validate;

// ── Test FormRequests ────────────────────────────────────────────────────────

/// Uses the default cap (process-global → compile-time default).
#[derive(Deserialize, Validate)]
struct DefaultForm {
    #[allow(dead_code)] // assertions are on status, not the parsed value
    payload: String,
}

impl FormRequest for DefaultForm {}

/// Overrides the cap to 32 MiB for legitimate bulk-ingest endpoints.
#[derive(Deserialize, Validate)]
struct LargeForm {
    #[allow(dead_code)]
    payload: String,
}

impl FormRequest for LargeForm {
    fn max_body_bytes() -> usize {
        32 * 1024 * 1024
    }
}

/// Override via the derive macro's struct-level attribute. Verifies the
/// derive composes with `#[form_request(max_body_bytes = N)]` (users
/// can't write a separate `impl FormRequest` block — that conflicts with
/// the one the derive emits — so the macro attribute is the supported
/// path for derive users).
#[derive(Deserialize, Validate, suprnova::FormRequestDerive)]
#[form_request(max_body_bytes = 4 * 1024 * 1024)] // 4 MiB
struct TinyDerivedForm {
    #[allow(dead_code)]
    payload: String,
}

// ── Serial-execution guard ───────────────────────────────────────────────────
//
// These tests mutate a process-global atomic; Cargo runs integration tests in
// parallel within the same binary by default. The guard pattern mirrors
// `BodyCapGuard` in `uploads.rs` — same poison-tolerant Mutex, same RAII
// reset-to-default-on-drop so a panicking test doesn't leak overrides into the
// next.

static REQ_BODY_CAP_LOCK: Mutex<()> = Mutex::new(());

struct ReqBodyCapGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl ReqBodyCapGuard {
    fn acquire() -> Self {
        let guard = REQ_BODY_CAP_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Always start from the compile-time default.
        suprnova::http::body::set_global_max_request_body_bytes(0);
        Self { _guard: guard }
    }
}

impl Drop for ReqBodyCapGuard {
    fn drop(&mut self) {
        suprnova::http::body::set_global_max_request_body_bytes(0);
    }
}

// Build a `{"payload": "xxx..."}` JSON body of `payload_len` characters.
fn json_payload(payload_len: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(payload_len + 16);
    buf.extend_from_slice(b"{\"payload\":\"");
    buf.resize(buf.len() + payload_len, b'x');
    buf.extend_from_slice(b"\"}");
    buf
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn default_cap_rejects_oversize_payload() {
    let _g = ReqBodyCapGuard::acquire();

    // 9 MiB payload — clears the 8 MiB compile-time default.
    let body = json_payload(9 * 1024 * 1024);
    let req = request_with_body("/", "application/json", &body).await;

    let err = DefaultForm::extract(req)
        .await
        .err()
        .expect("default 8 MiB cap should reject 9 MiB body");
    assert_eq!(err.status_code(), 413, "expected 413 Payload Too Large");
}

#[tokio::test]
async fn content_length_pre_check_rejects_without_reading_body() {
    let _g = ReqBodyCapGuard::acquire();

    // Declare 20 MiB in the header but send a 1 KiB body — pre-check fires
    // on the header alone, so the server never reads past the headers.
    let small_body = json_payload(1024);
    let req =
        request_with_declared_length("/", "application/json", 20 * 1024 * 1024, &small_body).await;

    let err = DefaultForm::extract(req)
        .await
        .err()
        .expect("Content-Length above cap should reject before body read");
    assert_eq!(err.status_code(), 413);
}

#[tokio::test]
async fn per_form_request_override_allows_larger_body() {
    let _g = ReqBodyCapGuard::acquire();

    // 12 MiB payload — over the default 8 MiB cap, under LargeForm's 32 MiB.
    let body = json_payload(12 * 1024 * 1024);
    let req = request_with_body("/", "application/json", &body).await;

    let result = LargeForm::extract(req).await;
    assert!(
        result.is_ok(),
        "12 MiB body should succeed under LargeForm's 32 MiB cap: {:?}",
        result.err().map(|e| e.status_code())
    );
}

#[tokio::test]
async fn global_override_raises_default_for_unannotated_form() {
    let _g = ReqBodyCapGuard::acquire();

    // Raise the process-global to 16 MiB. DefaultForm has no override, so
    // it now inherits 16 MiB. A 10 MiB body — previously rejected by the
    // 8 MiB default — should now succeed.
    suprnova::http::body::set_global_max_request_body_bytes(16 * 1024 * 1024);

    let body = json_payload(10 * 1024 * 1024);
    let req = request_with_body("/", "application/json", &body).await;

    let result = DefaultForm::extract(req).await;
    assert!(
        result.is_ok(),
        "10 MiB body should succeed under raised 16 MiB global cap: {:?}",
        result.err().map(|e| e.status_code())
    );
}

#[tokio::test]
async fn pre_check_message_includes_cap_bytes() {
    // The 413 error message must include the cap so operators can grep
    // logs to identify the source of the rejection (matches the
    // multipart cap's `"multipart body exceeds N bytes (cap)"` shape).
    let _g = ReqBodyCapGuard::acquire();

    let small_body = json_payload(1024);
    let req =
        request_with_declared_length("/", "application/json", 20 * 1024 * 1024, &small_body).await;

    let err = DefaultForm::extract(req)
        .await
        .err()
        .expect("pre-check should reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("request body exceeds") && msg.contains(&format!("{}", 8 * 1024 * 1024)),
        "413 message must include cap bytes for diagnostics: {msg}"
    );
}

#[tokio::test]
async fn progressive_cap_catches_chunked_without_content_length() {
    let _g = ReqBodyCapGuard::acquire();

    // Cap at 1 MiB. Send a chunked body in 256 KiB pieces, totalling 2 MiB.
    // No Content-Length is sent (chunked transfers don't carry one), so
    // the pre-check has nothing to act on; the cap has to enforce during
    // read.
    suprnova::http::body::set_global_max_request_body_bytes(1024 * 1024);

    // Eight 256 KiB chunks of `x` interleaved with the JSON wrapper.
    // Build as raw bytes so we don't have to worry about UTF-8 boundary
    // issues across chunk splits.
    let total_payload = 2 * 1024 * 1024usize;
    let full_body = json_payload(total_payload);
    let chunk_size = 256 * 1024;
    let mut chunks: Vec<&[u8]> = Vec::new();
    let mut offset = 0;
    while offset < full_body.len() {
        let end = (offset + chunk_size).min(full_body.len());
        chunks.push(&full_body[offset..end]);
        offset = end;
    }

    let req = request_with_chunked_body("/", "application/json", &chunks).await;
    let err = DefaultForm::extract(req)
        .await
        .err()
        .expect("progressive cap must catch chunked body that overruns");
    assert_eq!(err.status_code(), 413);
}

#[tokio::test]
async fn derive_with_form_request_attribute_lowers_cap() {
    let _g = ReqBodyCapGuard::acquire();

    // TinyDerivedForm caps itself at 4 MiB via the macro attribute. A 5 MiB
    // body — well under the 8 MiB default — should still be rejected
    // because the per-struct override wins.
    let body = json_payload(5 * 1024 * 1024);
    let req = request_with_body("/", "application/json", &body).await;

    let err = TinyDerivedForm::extract(req)
        .await
        .err()
        .expect("derive-attribute 4 MiB cap should reject 5 MiB body");
    assert_eq!(err.status_code(), 413);
}

#[tokio::test]
async fn derive_with_form_request_attribute_accepts_in_range_body() {
    let _g = ReqBodyCapGuard::acquire();

    // 1 MiB body — under TinyDerivedForm's 4 MiB cap.
    let body = json_payload(1024 * 1024);
    let req = request_with_body("/", "application/json", &body).await;

    let result = TinyDerivedForm::extract(req).await;
    assert!(
        result.is_ok(),
        "1 MiB body should succeed under TinyDerivedForm's 4 MiB cap: {:?}",
        result.err().map(|e| e.status_code())
    );
}

#[tokio::test]
async fn under_cap_request_succeeds() {
    let _g = ReqBodyCapGuard::acquire();

    // Sanity check: a small body well under the default cap parses cleanly.
    let body = json_payload(1024);
    let req = request_with_body("/", "application/json", &body).await;

    let result = DefaultForm::extract(req).await;
    assert!(
        result.is_ok(),
        "small body must succeed under default cap: {:?}",
        result.err().map(|e| e.status_code())
    );
}
