//! Streaming multipart upload support.
//!
//! This is a Task 4 stub: it pins the public API surface (types and the
//! `parse_multipart_streaming` entry point) so the test harness in
//! `framework/tests/common.rs` and `framework/tests/uploads.rs` can compile.
//! Task 5 replaces the stub body with the real streaming parser.
//
// The `#[allow(...)]` attributes below cover unused fields/parameters in
// the stub only; they fall away once Task 5 wires in the real parser.

use crate::error::FrameworkError;
use bytes::Bytes;

/// Output of [`parse_multipart_streaming`].
///
/// `fields` is an order-preserving `Vec` so duplicate names (e.g. the
/// `photos[]` PHP-array convention used by upstream Laravel) round-trip
/// in the same order the client sent them.
#[derive(Default)]
pub struct MultipartPayload {
    pub fields: Vec<(String, MultipartValue)>,
}

/// A single decoded multipart part — either a file or a plain text field.
#[allow(dead_code)] // variants are constructed by Task 5's real parser
pub enum MultipartValue {
    File {
        bytes: Bytes,
        file_name: Option<String>,
        content_type: Option<String>,
    },
    Text(String),
}

/// Task 4 stub — full implementation lands in Task 5.
///
/// Once Task 5 ships, this will stream each multipart part through
/// `per_field_validator` and assemble a [`MultipartPayload`].
#[allow(unused)]
pub async fn parse_multipart_streaming<F>(
    _req: crate::http::Request,
    _per_field_validator: F,
) -> Result<MultipartPayload, FrameworkError>
where
    F: FnMut(&str, &[u8]) -> Result<(), FrameworkError>,
{
    Err(FrameworkError::internal(
        "parse_multipart_streaming is stubbed; lands in Task 5",
    ))
}
