//! Body parsing utilities for HTTP requests
//!
//! Provides async body collection and parsing for JSON and form-urlencoded data.

use crate::error::FrameworkError;
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use serde::de::DeserializeOwned;

/// Collect the full body from an Incoming stream
pub async fn collect_body(body: Incoming) -> Result<Bytes, FrameworkError> {
    body.collect()
        .await
        .map(|collected| collected.to_bytes())
        .map_err(|e| FrameworkError::internal(format!("Failed to read request body: {}", e)))
}

/// Parse bytes as JSON into the target type
///
/// Deserialization errors map to 422 Unprocessable Entity — the client
/// supplied invalid input (wrong shape, rejected fields, bad types).
pub fn parse_json<T: DeserializeOwned>(bytes: &Bytes) -> Result<T, FrameworkError> {
    serde_json::from_slice(bytes)
        .map_err(|e| FrameworkError::domain(format!("Failed to parse JSON body: {}", e), 422))
}

/// Parse bytes as form-urlencoded into the target type
///
/// Deserialization errors map to 422 Unprocessable Entity — the client
/// supplied invalid input.
pub fn parse_form<T: DeserializeOwned>(bytes: &Bytes) -> Result<T, FrameworkError> {
    serde_urlencoded::from_bytes(bytes)
        .map_err(|e| FrameworkError::domain(format!("Failed to parse form body: {}", e), 422))
}
