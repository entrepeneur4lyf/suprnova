//! In-memory recorder and canned-response store for `Http::fake()`.
//!
//! Activated by `Http::fake(|| async { ... }).await`: the closure runs
//! inside a `tokio::task_local!` scope where every `RequestBuilder::send`
//! is intercepted, captured into a recorded-requests vec, and matched
//! against canned responses queued via [`fake_response`].
//!
//! Each task gets its own isolated fake state (`Arc<Mutex<FakeState>>`),
//! so tests can run in parallel without serializing themselves on a
//! process-wide mutex.
//!
//! Note: `tokio::task_local!` is task-scoped. Work spawned via
//! `tokio::spawn` inside the scope runs on a fresh task and does NOT
//! inherit the fake by default — those requests escape to the real
//! network (or fail-closed when `Http::fail_on_real_calls()` is on).
//!
//! For the cases that actually want spawned tasks to share the parent's
//! fake (e.g. a queue worker that calls outbound HTTP from within a
//! spawned future), [`Http::spawn_with_fake_inheritance`] captures the
//! current fake state via the shared `Arc` and re-installs it in the
//! child's `tokio::task_local!` scope. Recorded requests and consumed
//! canned responses are shared with the parent through the same Arc —
//! `assert_sent` on the parent sees what the child sent.

use crate::lock;
use std::future::Future;
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use super::{Body, ClientResponse, RequestBuilder};

tokio::task_local! {
    /// Per-task fake state. Set by [`Http::fake`]. Inside the scope,
    /// `is_fake_active()` returns `true` and `intercept` reads / mutates
    /// the state. Outside, all of them return `false` / panic with a
    /// friendly error.
    ///
    /// `Arc<Mutex<...>>` (rather than `Mutex<...>` directly) so the
    /// state can be cloned cheaply and re-installed in spawned tasks
    /// via [`Http::spawn_with_fake_inheritance`]; recorded requests
    /// from the child remain visible to the parent through the same
    /// Arc.
    static FAKE_STATE: Arc<Mutex<FakeState>>;
}

#[derive(Default)]
pub(crate) struct FakeState {
    recorded: Vec<RecordedRequest>,
    canned: Vec<CannedResponse>,
}

/// A recorded outbound request — used by [`assert_sent`] /
/// [`assert_not_sent`].
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// HTTP method as a static string: `"GET"`, `"POST"`, etc.
    pub method: String,
    /// Full request URL exactly as passed to `Http::get`/`post`/etc.
    pub url: String,
    /// Headers added to the request, in the order they were appended.
    pub headers: Vec<(String, String)>,
    /// Raw body bytes (JSON serialized as JSON, form serialized as
    /// urlencoded, raw passed through).
    pub body: Option<Vec<u8>>,
}

struct CannedResponse {
    method: String,
    url_substring: String,
    status: u16,
    body: Bytes,
}

/// Queue a canned response. The first request whose method matches
/// (case-insensitive) and whose URL contains `url_substring` returns
/// this response — and the canned entry is consumed.
///
/// Method `"*"` matches any method.
///
/// Subsequent matching requests fall through to the next canned entry,
/// or — if none match — return an empty `200 {}`.
///
/// **Must be called inside a `Http::fake(|| async { ... })` scope.**
/// Panics if no fake scope is active on the current task.
pub fn fake_response(method: &str, url_substring: &str, status: u16, body: serde_json::Value) {
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    with_state(|s| {
        s.canned.push(CannedResponse {
            method: method.to_string(),
            url_substring: url_substring.to_string(),
            status,
            body: Bytes::from(bytes),
        });
    });
}

/// Assert that at least one recorded request satisfies the predicate.
/// Panics with a list of recorded requests on failure.
///
/// Must be called inside a `Http::fake(...)` scope.
pub fn assert_sent(predicate: impl Fn(&RecordedRequest) -> bool) {
    with_state(|s| {
        if !s.recorded.iter().any(&predicate) {
            panic!(
                "assert_sent: no recorded request matched the predicate. \
                 Recorded: {:#?}",
                s.recorded
            );
        }
    });
}

/// Assert that no recorded request satisfies the predicate. Panics
/// with the offending request on failure.
///
/// Must be called inside a `Http::fake(...)` scope.
pub fn assert_not_sent(predicate: impl Fn(&RecordedRequest) -> bool) {
    with_state(|s| {
        if let Some(hit) = s.recorded.iter().find(|r| predicate(r)) {
            panic!("assert_not_sent: forbidden request was sent: {:#?}", hit);
        }
    });
}

/// Run `f` inside a task-local fake scope. While `f` is awaiting, every
/// outbound HTTP call on the same task is intercepted instead of
/// hitting the network.
///
/// Returns whatever the closure returns.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{Http, fake_response, assert_sent};
///
/// Http::fake(|| async {
///     fake_response("POST", "/api/users", 201, serde_json::json!({"id": 1}));
///     let resp = Http::post("https://example.com/api/users")
///         .json(&serde_json::json!({"name": "Ada"}))
///         .send()
///         .await
///         .unwrap();
///     assert_eq!(resp.status(), 201);
///     assert_sent(|r| r.method == "POST" && r.url.contains("/api/users"));
/// })
/// .await;
/// ```
pub async fn install_fake_scope<F, Fut, T>(f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = T>,
{
    FAKE_STATE
        .scope(Arc::new(Mutex::new(FakeState::default())), f())
        .await
}

/// Install a previously-captured `Arc<Mutex<FakeState>>` for the
/// duration of `fut`, so a spawned task can share the parent's
/// recorded-requests / canned-responses store. Used by
/// [`Http::spawn_with_fake_inheritance`].
pub(crate) async fn install_inherited_scope<F, T>(state: Arc<Mutex<FakeState>>, fut: F) -> T
where
    F: Future<Output = T>,
{
    FAKE_STATE.scope(state, fut).await
}

/// Capture the current task's fake state for re-installation in a
/// spawned task. Returns `None` when no fake scope is active on the
/// current task — in which case the caller should fall through to a
/// regular `tokio::spawn` rather than asserting inheritance.
pub(crate) fn snapshot_current_fake_state() -> Option<Arc<Mutex<FakeState>>> {
    FAKE_STATE.try_with(|state| state.clone()).ok()
}

/// `true` if a fake scope is active on the current task. Used by
/// `RequestBuilder::send` to decide whether to short-circuit.
pub(crate) fn is_fake_active() -> bool {
    // `try_with` returns Ok if the task_local is in scope.
    FAKE_STATE.try_with(|_| ()).is_ok()
}

pub(crate) fn intercept(req: &RequestBuilder) -> ClientResponse {
    let body_bytes = match &req.body {
        Some(Body::Json(v)) => Some(serde_json::to_vec(v).unwrap_or_default()),
        Some(Body::Form(v)) => Some(
            serde_urlencoded::to_string(v)
                .unwrap_or_default()
                .into_bytes(),
        ),
        Some(Body::Raw(b)) => Some(b.to_vec()),
        None => None,
    };

    with_state(|s| {
        s.recorded.push(RecordedRequest {
            method: req.method.as_str().to_string(),
            url: req.url.clone(),
            headers: req.headers.clone(),
            body: body_bytes,
        });

        let method_str = req.method.as_str();
        let idx = s.canned.iter().position(|c| {
            let m_ok = c.method == "*" || c.method.eq_ignore_ascii_case(method_str);
            m_ok && req.url.contains(&c.url_substring)
        });

        match idx {
            Some(i) => {
                let c = s.canned.remove(i);
                ClientResponse::fake(
                    c.status,
                    vec![("content-type".to_string(), "application/json".to_string())],
                    c.body,
                )
            }
            None => ClientResponse::fake(
                200,
                vec![("content-type".to_string(), "application/json".to_string())],
                Bytes::from_static(b"{}"),
            ),
        }
    })
}

/// Access the per-task `FakeState`. Panics if no scope is active.
fn with_state<R>(f: impl FnOnce(&mut FakeState) -> R) -> R {
    FAKE_STATE
        .try_with(|state| {
            let mut guard = lock::lock(state).expect("FakeState mutex poisoned");
            f(&mut guard)
        })
        .unwrap_or_else(|_| {
            panic!(
                "Http fake helpers called outside an Http::fake(...) scope. \
                 Wrap the test body in Http::fake(|| async {{ ... }}).await."
            )
        })
}
