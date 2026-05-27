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
pub struct RequestIdMiddleware {
    /// A pre-resolved id, or `None` to resolve from the request at
    /// handle time. See [`RequestIdMiddleware::new`] and
    /// [`RequestIdMiddleware::with_id`].
    id: Option<RequestId>,
}

impl RequestIdMiddleware {
    /// Self-resolving install: the middleware reads `X-Request-Id` from
    /// the inbound request (reusing it when safe) or mints a fresh UUID
    /// v4 at request time. This is the standalone form for installs that
    /// have no pre-resolved id to share.
    pub fn new() -> Self {
        Self { id: None }
    }

    /// Install with an id already resolved via [`resolve_request_id`].
    ///
    /// The server resolves the request id ONCE and shares it here so the
    /// panic boundary (`execute_chain_safely`) can echo the SAME id on a
    /// synthesized 500. A panic unwinds the `REQUEST_ID` scope, so by the
    /// time the panic is caught the id is no longer recoverable from the
    /// task-local — it must be threaded in from outside the chain.
    pub fn with_id(id: RequestId) -> Self {
        Self { id: Some(id) }
    }
}

impl Default for RequestIdMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

/// Maximum length we accept for an inbound `X-Request-Id` header.
/// 128 bytes is plenty for UUIDs (36), ULIDs (26), KSUIDs (27), and
/// most distributed-tracing systems' trace ids. Anything longer is
/// rejected and replaced with a fresh UUID so an attacker cannot use
/// the header to inject control characters into log output, balloon
/// the Context bag, or cause downstream pipelines to choke on
/// pathologically long ids.
const MAX_REQUEST_ID_LEN: usize = 128;

/// Returns true if `s` is a safe id: ASCII printable, no control
/// characters, no whitespace, within the length cap. Most production
/// id formats (UUID, ULID, KSUID, hex trace ids) satisfy this.
fn is_safe_request_id(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_REQUEST_ID_LEN {
        return false;
    }
    s.bytes().all(|b| b.is_ascii_graphic() && b != b' ')
}

/// Resolve the request id for an inbound request: reuse a safe
/// `X-Request-Id` header when present, otherwise mint a fresh UUID v4.
///
/// Extracted from the middleware so the server can resolve the id ONCE
/// per request and share it between the middleware (which scopes it for
/// the request) and the panic boundary (which must echo the SAME id on a
/// synthesized 500 even though the request scope has already unwound).
pub(crate) fn resolve_request_id(request: &Request) -> RequestId {
    request
        .header("x-request-id")
        .filter(|s| is_safe_request_id(s))
        .map(RequestId::from_string)
        .unwrap_or_default()
}

#[async_trait]
impl crate::middleware::Middleware for RequestIdMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Use the id the server pre-resolved for this request when
        // present; otherwise resolve it here (standalone installs).
        let id = self
            .id
            .clone()
            .unwrap_or_else(|| resolve_request_id(&request));
        let id_str = id.as_str().to_string();
        let id_for_context = id_str.clone();

        // Snapshot the request's query parameters into a `HashMap` so
        // `Context::query_param` and downstream paginate / cursor code
        // can read them without re-parsing the URI on every call.
        //
        // Audit HIGH #333: prior to this snapshot the middleware used
        // `ContextStore::default()`, so the in-scope `query` bag was
        // always empty for real HTTP requests — `Context::query_param`
        // always returned `None` and Eloquent pagination silently
        // defaulted to page 1 / no-cursor regardless of `?page=` or
        // `?cursor=` in the URL.
        let query_map: std::collections::HashMap<String, String> = request
            .query()
            .map(|q| {
                url::form_urlencoded::parse(q.as_bytes())
                    .into_owned()
                    .collect()
            })
            .unwrap_or_default();

        // Scope both REQUEST_ID and CONTEXT for the rest of the
        // request. The CONTEXT scope seeds `_request_id` so downstream
        // log emitters / jobs / broadcasting can read the id by name,
        // and the query snapshot so `Context::query_param` returns the
        // real URL's `?key=value` pairs (last-wins on duplicate keys,
        // matching Laravel semantics).
        let result = crate::context::CONTEXT
            .scope(
                crate::context::ContextStore::with_query(query_map),
                async move {
                    crate::context::Context::add("_request_id", id_for_context);
                    REQUEST_ID
                        .scope(id, async move { next(request).await })
                        .await
                },
            )
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
        assert!(
            id.as_str()
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }
}
