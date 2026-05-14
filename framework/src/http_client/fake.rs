//! In-memory recorder and canned-response store for `Http::fake()`.
//!
//! Activated by `Http::fake()` returning an [`HttpFakeGuard`]. While the
//! guard is alive every `RequestBuilder::send` is intercepted, captured
//! into a recorded-requests vec, and matched against canned responses
//! queued via [`fake_response`].
//!
//! The state is a process-wide `Mutex<Option<FakeState>>`. Tests that
//! exercise the fake must serialize themselves via a local `Mutex<()>`
//! because the recorder is global.

use std::sync::Mutex;

use bytes::Bytes;

use super::{Body, ClientResponse, RequestBuilder};

static FAKE_STATE: Mutex<Option<FakeState>> = Mutex::new(None);

struct FakeState {
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
pub fn fake_response(
    method: &str,
    url_substring: &str,
    status: u16,
    body: serde_json::Value,
) {
    let mut state = FAKE_STATE.lock().unwrap();
    if let Some(s) = state.as_mut() {
        let bytes = serde_json::to_vec(&body).unwrap_or_default();
        s.canned.push(CannedResponse {
            method: method.to_string(),
            url_substring: url_substring.to_string(),
            status,
            body: Bytes::from(bytes),
        });
    } else {
        panic!(
            "fake_response called without an active Http::fake() guard"
        );
    }
}

/// Assert that at least one recorded request satisfies the predicate.
/// Panics with a list of recorded requests on failure.
pub fn assert_sent(predicate: impl Fn(&RecordedRequest) -> bool) {
    let state = FAKE_STATE.lock().unwrap();
    let Some(s) = state.as_ref() else {
        panic!("assert_sent called without an active Http::fake() guard");
    };
    if !s.recorded.iter().any(predicate) {
        panic!(
            "assert_sent: no recorded request matched the predicate. \
             Recorded: {:#?}",
            s.recorded
        );
    }
}

/// Assert that no recorded request satisfies the predicate. Panics
/// with the offending request on failure.
pub fn assert_not_sent(predicate: impl Fn(&RecordedRequest) -> bool) {
    let state = FAKE_STATE.lock().unwrap();
    let Some(s) = state.as_ref() else {
        panic!("assert_not_sent called without an active Http::fake() guard");
    };
    if let Some(hit) = s.recorded.iter().find(|r| predicate(*r)) {
        panic!("assert_not_sent: forbidden request was sent: {:#?}", hit);
    }
}

/// Drop-guard returned by [`Http::fake`]. Restores normal HTTP behavior
/// when dropped.
pub struct HttpFakeGuard;

impl Drop for HttpFakeGuard {
    fn drop(&mut self) {
        *FAKE_STATE.lock().unwrap() = None;
    }
}

pub(crate) fn install_fake() -> HttpFakeGuard {
    let mut state = FAKE_STATE.lock().unwrap();
    *state = Some(FakeState {
        recorded: Vec::new(),
        canned: Vec::new(),
    });
    HttpFakeGuard
}

pub(crate) fn is_fake_active() -> bool {
    FAKE_STATE.lock().unwrap().is_some()
}

pub(crate) fn intercept(req: &RequestBuilder) -> ClientResponse {
    let mut state = FAKE_STATE.lock().unwrap();
    let s = state
        .as_mut()
        .expect("intercept called while fake inactive — racy install/drop");

    // Capture the request.
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
    s.recorded.push(RecordedRequest {
        method: req.method.as_str().to_string(),
        url: req.url.clone(),
        headers: req.headers.clone(),
        body: body_bytes,
    });

    // Find a canned response.
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
}
