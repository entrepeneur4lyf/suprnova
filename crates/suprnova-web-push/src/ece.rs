//! AES128GCM (RFC 8291) implementation lives in the `ece` crate directly;
//! the `Payload` type in `crate::payload` wraps it for use by the web-push
//! client. This module is intentionally minimal — re-exports only.

pub use crate::payload::{ContentEncoding, MAX_PLAINTEXT_BYTES, Payload};
