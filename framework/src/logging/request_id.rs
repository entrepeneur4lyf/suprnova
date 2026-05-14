//! Per-request UUID stored in a `tokio::task_local!`. The
//! `RequestIdMiddleware` installs it as the outermost middleware so
//! every downstream `tracing` event, every error log, and every event
//! payload carries the same id.

use std::fmt;
use uuid::Uuid;

/// A request id: lowercase hyphenated UUID v4.
#[derive(Debug, Clone)]
pub struct RequestId(String);

impl RequestId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

tokio::task_local! {
    pub static REQUEST_ID: RequestId;
}

/// Returns the request id of the currently-executing task, if any.
///
/// `None` outside an active `REQUEST_ID::scope` (i.e. background
/// jobs, tests that didn't install the middleware, etc.).
pub fn current_request_id() -> Option<RequestId> {
    REQUEST_ID.try_with(|id| id.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn current_request_id_outside_scope_returns_none() {
        assert!(current_request_id().is_none());
    }

    #[tokio::test]
    async fn current_request_id_inside_scope_returns_value() {
        let id = RequestId::new();
        let captured = id.clone();
        REQUEST_ID
            .scope(id, async move {
                let now = current_request_id().expect("scoped value present");
                assert_eq!(now.as_str(), captured.as_str());
            })
            .await;
    }

    #[tokio::test]
    async fn request_id_is_lowercase_hyphenated_uuid() {
        let id = RequestId::new();
        assert_eq!(id.as_str().len(), 36);
        assert_eq!(id.as_str().chars().filter(|c| *c == '-').count(), 4);
        assert!(id
            .as_str()
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    }
}
