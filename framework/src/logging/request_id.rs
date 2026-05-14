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

use crate::http::{Request, Response};
use crate::middleware::Next;
use async_trait::async_trait;

/// Middleware that ensures every request has a `RequestId` scoped in
/// `REQUEST_ID`. If the inbound request carries an `X-Request-Id`
/// header, that value is reused; otherwise a fresh UUID v4 is
/// generated. The id is echoed back as `X-Request-Id` on the response
/// (both success and error variants).
pub struct RequestIdMiddleware;

#[async_trait]
impl crate::middleware::Middleware for RequestIdMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let id = request
            .header("x-request-id")
            .map(RequestId::from_string)
            .unwrap_or_else(RequestId::new);
        let id_str = id.as_str().to_string();
        let id_for_context = id_str.clone();

        // Scope both REQUEST_ID and CONTEXT for the rest of the
        // request. The CONTEXT scope seeds `_request_id` so downstream
        // log emitters / jobs / broadcasting can read the id by name.
        let result = crate::context::CONTEXT
            .scope(crate::context::ContextStore::default(), async move {
                crate::context::Context::add("_request_id", id_for_context);
                REQUEST_ID
                    .scope(id, async move { next(request).await })
                    .await
            })
            .await;

        // Echo X-Request-Id on both success and error variants.
        match result {
            Ok(resp) => Ok(resp.header("X-Request-Id", id_str)),
            Err(resp) => Err(resp.header("X-Request-Id", id_str)),
        }
    }
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
