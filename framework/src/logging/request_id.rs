//! Per-request id stored in a `tokio::task_local!` and attached to a
//! `tracing` span. `RequestIdMiddleware` installs it as the outermost
//! middleware and enters a `request` span — carrying `request_id`,
//! `method`, and `path` — around the rest of the chain, so every
//! downstream `tracing` event is emitted within that span and carries
//! the id as span context (nested under `span` in the JSON formatter).
//! The id is also seeded into the request `Context` (`_request_id`) so
//! error logs, jobs, and event payloads can read it by name.

use std::fmt;
use tokio::task::JoinHandle;
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

/// Spawn `future` onto the Tokio runtime, propagating the current
/// request id into the new task.
///
/// `tokio::spawn` starts a task with empty task-locals, so a handler that
/// spawns background work loses `current_request_id()` — the spawned
/// work's logs and any id-derived correlation would be orphaned from the
/// request that triggered them. This helper captures the caller's request
/// id and re-scopes it for the spawned future, and attaches the current
/// `tracing` span so the spawned task's events inherit `request_id` the
/// same way in-request events do. Use it for background side effects,
/// queued event tasks, and audit logging kicked off mid-request.
///
/// With no active request id (called outside a request) the future is
/// spawned as-is, exactly like a bare `tokio::spawn`.
///
/// Note: only the request id and tracing span follow the task — the
/// request `Context` bag (query params, flash) deliberately does not,
/// since background work is not serving the originating HTTP request.
pub fn spawn_with_request_id<F>(future: F) -> JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    match current_request_id() {
        Some(id) => {
            let span = tracing::Span::current();
            tokio::spawn(REQUEST_ID.scope(id, future).instrument(span))
        }
        None => tokio::spawn(future),
    }
}

use crate::http::{Request, Response};
use crate::middleware::Next;
use async_trait::async_trait;
use tracing::Instrument;

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

        // The request span carries `request_id`, `method`, and `path` as
        // fields. Entering it (via `.instrument` below) around the rest of
        // the chain means every downstream `tracing` event inherits the id
        // as span context — without each call site having to read and
        // record it. Fields are recorded eagerly here, so the borrows of
        // `request` end before it is moved into the handler.
        let span = tracing::info_span!(
            "request",
            request_id = %id_str,
            method = %request.method(),
            path = %request.path(),
            // Declared up front as Empty so the 5xx marker recorded after
            // the chain actually lands — `tracing` silently ignores
            // `record` for fields not declared at span creation. The field
            // name is the one `tracing-opentelemetry` special-cases:
            // recording `otel.status_code = "error"` makes the bridge call
            // `span.set_status(Status::error(..))`, which is the real OTel
            // span status (a bare `error` field would only become a plain
            // attribute, not a Status). A never-recorded Empty field is
            // omitted from output, so this is zero-cost on the 2xx path and
            // in non-otel builds.
            otel.status_code = tracing::field::Empty,
        );

        // Join the upstream distributed trace before the span is entered.
        // If the request carries a valid W3C `traceparent`, this reparents
        // the span onto the caller's span so the server span is a child in
        // the same trace instead of a fresh root. No-op without the `otel`
        // feature or without a usable trace header. MUST run before
        // `.instrument(span)` — the OTel bridge materializes the span on
        // first poll, so a later `set_parent` is dropped.
        crate::telemetry::propagation::join_upstream_trace(&span, request.headers());

        // Clone for the post-chain 5xx error marker; the original `span` is
        // moved into `.instrument(...)` below. Only needed under `otel`
        // (the field is otherwise never recorded), so the clone is gated to
        // avoid an unused binding in default builds.
        #[cfg(feature = "otel")]
        let span_for_status = span.clone();

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
                }
                .instrument(span),
            )
            .await;

        // Mark the span errored on a 5xx response so the
        // `tracing-opentelemetry` bridge maps it to OTel `Status::Error`.
        // The value `"error"` on the `otel.status_code` field is what the
        // bridge matches (case-insensitively) to call `set_status`.
        // Recorded here — the outermost middleware — so all three
        // chain-running server paths (matched route, fallback, static 404)
        // get the marker from one place instead of three duplicated
        // post-chain blocks in `server.rs`.
        //
        // A *panic* inside the chain unwinds past this point (the span's
        // future is dropped mid-flight), so a panic-induced 500 is NOT
        // marked here; `execute_chain_safely` still emits an error-level
        // log and dispatches `ErrorOccurred` for that case, so it is not
        // silent — only the OTel span status is unset on panic.
        #[cfg(feature = "otel")]
        {
            let status = match &result {
                Ok(resp) | Err(resp) => resp.status_code(),
            };
            if status >= 500 {
                span_for_status.record("otel.status_code", "error");
            }
        }

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

    #[tokio::test]
    async fn spawn_with_request_id_propagates_into_the_spawned_task() {
        use std::sync::{Arc, Mutex};

        let captured = Arc::new(Mutex::new(None::<String>));
        let sink = captured.clone();

        let id = RequestId::from_string("spawn-propagation-id-555");
        REQUEST_ID
            .scope(id, async move {
                // Background work spawned mid-request must still observe the id.
                let handle = spawn_with_request_id(async move {
                    *sink.lock().unwrap() = current_request_id().map(|r| r.as_str().to_string());
                });
                handle.await.unwrap();
            })
            .await;

        assert_eq!(
            captured.lock().unwrap().as_deref(),
            Some("spawn-propagation-id-555"),
            "the spawned task must inherit the caller's request id"
        );
    }

    #[tokio::test]
    async fn spawn_with_request_id_outside_a_scope_carries_no_id() {
        use std::sync::{Arc, Mutex};

        // Seed with a sentinel so we can tell "set to None" from "never ran".
        let captured = Arc::new(Mutex::new(Some("sentinel".to_string())));
        let sink = captured.clone();

        let handle = spawn_with_request_id(async move {
            *sink.lock().unwrap() = current_request_id().map(|r| r.as_str().to_string());
        });
        handle.await.unwrap();

        assert_eq!(
            captured.lock().unwrap().as_deref(),
            None,
            "with no active request id, the spawned task gets none (bare spawn)"
        );
    }
}
