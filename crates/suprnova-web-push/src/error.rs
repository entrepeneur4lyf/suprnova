//! Errors for the web-push crate.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WebPushError {
    #[error("VAPID key error: {0}")]
    Vapid(String),
    #[error("payload encryption failed: {0}")]
    Encryption(String),
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("push service rejected: status {status}, body: {body}")]
    PushServiceRejected { status: u16, body: String },
    #[error("base64 decode: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("JSON encode/decode: {0}")]
    Json(#[from] serde_json::Error),
    #[error("subscription expired or invalid (HTTP 404/410)")]
    SubscriptionGone,
    #[error("internal: {0}")]
    Internal(String),
}
