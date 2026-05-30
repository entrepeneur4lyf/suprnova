//! Errors for the web-push crate.

use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WebPushError {
    #[error("VAPID key error: {0}")]
    Vapid(String),
    #[error("payload encryption failed: {0}")]
    Encryption(String),
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),
    /// The push service responded with a non-2xx, non-404/410 status.
    ///
    /// `retry_after` is populated when the push service supplied an
    /// RFC 7231 `Retry-After` header in delta-seconds form. The HTTP-date
    /// form is recognised but not parsed — callers that need date-form
    /// support should re-fetch the header themselves; the overwhelming
    /// majority of push services (FCM, Mozilla AutoPush, APNs HTTP/2 web
    /// push) emit delta-seconds.
    ///
    /// `body` is bounded to at most a few KiB so a hostile push service
    /// can't drive unbounded memory growth by streaming a huge error body.
    /// See [`PushServiceRejected::is_retryable`] for whether a caller
    /// should retry vs. drop.
    #[error(
        "push service rejected: status {status}{retry_hint}, body: {body}",
        retry_hint = retry_after
            .map(|d| format!(", retry-after {}s", d.as_secs()))
            .unwrap_or_default()
    )]
    PushServiceRejected {
        status: u16,
        retry_after: Option<Duration>,
        body: String,
    },
    #[error("base64 decode: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("JSON encode/decode: {0}")]
    Json(#[from] serde_json::Error),
    #[error("subscription expired or invalid (HTTP 404/410)")]
    SubscriptionGone,
    #[error("internal: {0}")]
    Internal(String),
}

impl WebPushError {
    /// Whether a caller should retry the send after a transient failure.
    ///
    /// Returns `true` for HTTP transport errors (no response received —
    /// network/timeout/DNS), and for `PushServiceRejected` with a 408
    /// (request timeout), 429 (too many requests), or any 5xx status.
    /// Returns `false` for terminal outcomes: `SubscriptionGone` (404/410),
    /// other 4xx (authn/authz/protocol errors), and for the local errors
    /// that fired before any HTTP I/O (`Vapid`, `Encryption`, `Base64`,
    /// `Json`, `Internal`).
    ///
    /// When `Some`, [`Self::retry_after`] gives the push-service-suggested
    /// minimum delay.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http(_) => true,
            Self::PushServiceRejected { status, .. } => {
                matches!(*status, 408 | 429) || (500..=599).contains(status)
            }
            Self::SubscriptionGone
            | Self::Vapid(_)
            | Self::Encryption(_)
            | Self::Base64(_)
            | Self::Json(_)
            | Self::Internal(_) => false,
        }
    }

    /// The push service's suggested retry delay, if it sent one and the
    /// header was in delta-seconds form. Only meaningful when
    /// [`Self::is_retryable`] is `true`; absent retry hints return `None`.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::PushServiceRejected { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}
