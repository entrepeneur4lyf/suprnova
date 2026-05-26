//! Verify error responses are sanitised per codex review finding #2.
//!
//! Status >= 500: the body's `message` field is the generic
//! "Internal Server Error" string; the raw `err.to_string()` is NEVER
//! returned to clients. With `APP_DEBUG=true` (false-by-default outside
//! local/dev/test) the body additionally carries a `debug_message`
//! field — but `message` STAYS generic, so frontends never accidentally
//! key on the detail field.
//!
//! Status 4xx (domain errors): the original message is preserved
//! because it's caller-facing and meaningful ("Missing required
//! parameter: avatar", "field 'priority' must be a u32", etc.).
//!
//! All error bodies carry a `request_id` field — `null` outside a
//! request scope, the active `RequestId` inside one.
//!
//! # Test isolation
//!
//! These tests mutate `APP_DEBUG` (process-global env). We follow the
//! same pattern used in `tests/env_loading.rs`: a module-local `Mutex`
//! serialises every test in this file, and each test snapshots+restores
//! `APP_DEBUG` on drop so a failure doesn't poison the next test.

use std::sync::Mutex;

use serde_json::Value;
use suprnova::error::FrameworkError;
use suprnova::http::HttpResponse;

/// Serialise the suite — every test reads `AppConfig::from_env()`
/// inside the error-response path, which observes `APP_DEBUG` as a
/// process-global. Without the lock, a test that sets APP_DEBUG=true
/// races with a parallel test that sets APP_DEBUG=false.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Marshal a `FrameworkError` through the `IntoResponse` path and return
/// `(status, parsed_body)` for inspection.
fn render(err: FrameworkError) -> (u16, Value) {
    let resp: HttpResponse = err.into();
    let status = resp.status_code();
    // `HttpResponse` doesn't expose body bytes directly; we route it
    // through the hyper conversion that the server uses at runtime.
    let body = resp.body().to_vec();
    let parsed: Value = serde_json::from_slice(&body).expect("error response body is always JSON");
    (status, parsed)
}

/// RAII guard that (a) acquires the shared `ENV_LOCK`, (b) sets
/// `APP_DEBUG` to the requested value, and (c) restores the previous
/// `APP_DEBUG` value when dropped. Holding the lock for the guard's
/// lifetime serialises every env-mutating test in this file.
struct AppDebugGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<String>,
}

impl AppDebugGuard {
    fn new(value: &str) -> Self {
        // Recover from poisoned locks — a previous test panicking
        // shouldn't make the rest of the suite unrunnable.
        let lock = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let previous = std::env::var("APP_DEBUG").ok();
        // SAFETY: the env mutation is serialised through `ENV_LOCK`.
        // We restore on drop.
        unsafe { std::env::set_var("APP_DEBUG", value) };
        Self {
            _lock: lock,
            previous,
        }
    }

    fn falsy() -> Self {
        Self::new("false")
    }

    fn truthy() -> Self {
        Self::new("true")
    }
}

impl Drop for AppDebugGuard {
    fn drop(&mut self) {
        // SAFETY: still holding `ENV_LOCK` (it's a field).
        unsafe {
            match self.previous.as_deref() {
                Some(v) => std::env::set_var("APP_DEBUG", v),
                None => std::env::remove_var("APP_DEBUG"),
            }
        }
    }
}

#[test]
fn internal_error_500_body_is_generic_in_production() {
    let _g = AppDebugGuard::falsy();

    let err = FrameworkError::internal("DB error: connection refused at 127.0.0.1:5432");
    let (status, body) = render(err);

    assert_eq!(status, 500);
    assert_eq!(
        body["message"], "Internal Server Error",
        "5xx message must be generic — got: {body}"
    );
    assert!(
        body.get("debug_message").is_none(),
        "debug_message must be absent when APP_DEBUG=false — got: {body}"
    );
    // Raw error text must never appear anywhere in the body.
    let body_str = serde_json::to_string(&body).unwrap();
    assert!(
        !body_str.contains("127.0.0.1"),
        "raw error detail leaked into response body: {body_str}"
    );
}

#[test]
fn database_error_500_body_is_generic_in_production() {
    let _g = AppDebugGuard::falsy();

    let err = FrameworkError::database(
        "syntax error at or near 'SELCT' (column 1) — relation users does not exist",
    );
    let (status, body) = render(err);

    assert_eq!(status, 500);
    assert_eq!(body["message"], "Internal Server Error");
    assert!(body.get("debug_message").is_none());
    let body_str = serde_json::to_string(&body).unwrap();
    assert!(
        !body_str.contains("SELCT"),
        "database error detail leaked: {body_str}"
    );
    assert!(
        !body_str.contains("users"),
        "database error detail leaked: {body_str}"
    );
}

#[test]
fn service_not_found_500_body_is_generic_in_production() {
    let _g = AppDebugGuard::falsy();

    let err = FrameworkError::service_not_found::<u64>(); // arbitrary marker type
    let (status, body) = render(err);

    assert_eq!(status, 500);
    assert_eq!(body["message"], "Internal Server Error");
    assert!(body.get("debug_message").is_none());
    let body_str = serde_json::to_string(&body).unwrap();
    assert!(
        !body_str.contains("u64"),
        "service type name leaked into response: {body_str}"
    );
}

#[test]
fn internal_error_with_debug_exposes_debug_message_keeping_message_generic() {
    let _g = AppDebugGuard::truthy();

    let raw = "DB error: connection refused at 127.0.0.1:5432";
    let err = FrameworkError::internal(raw);
    let (status, body) = render(err);

    assert_eq!(status, 500);
    // `message` STAYS generic even in debug mode — frontends must never
    // be able to key on the detail field.
    assert_eq!(body["message"], "Internal Server Error");
    assert_eq!(
        body["debug_message"],
        format!("Internal server error: {raw}"),
        "debug_message should expose the full err.to_string() — got: {body}"
    );
}

#[test]
fn domain_4xx_preserves_original_message() {
    let _g = AppDebugGuard::falsy();

    let err = FrameworkError::Domain {
        message: "missing required file field 'avatar'".into(),
        status_code: 400,
    };
    let (status, body) = render(err);

    assert_eq!(status, 400);
    assert_eq!(
        body["message"], "missing required file field 'avatar'",
        "4xx must preserve caller-facing detail — got: {body}"
    );
    assert!(body.get("debug_message").is_none());
}

#[test]
fn param_error_4xx_keeps_field_specific_message() {
    let _g = AppDebugGuard::falsy();

    let err = FrameworkError::param("priority");
    let (status, body) = render(err);

    assert_eq!(status, 400);
    assert!(
        body["message"].as_str().unwrap().contains("priority"),
        "ParamError must preserve param name in message — got: {body}"
    );
}

#[test]
fn validation_4xx_keeps_per_field_errors() {
    let _g = AppDebugGuard::falsy();

    let mut errs = suprnova::error::ValidationErrors::new();
    errs.add("email", "The email field must be a valid email address.");
    let err = FrameworkError::validation_errors(errs);
    let (status, body) = render(err);

    assert_eq!(status, 422);
    assert_eq!(body["message"], "The given data was invalid.");
    assert!(
        body["errors"]["email"]
            .as_array()
            .map(|arr| !arr.is_empty())
            .unwrap_or(false),
        "validation errors must include per-field detail — got: {body}"
    );
}

#[test]
fn unauthorized_403_keeps_static_message() {
    let _g = AppDebugGuard::falsy();

    let err = FrameworkError::Unauthorized;
    let (status, body) = render(err);

    assert_eq!(status, 403);
    assert_eq!(body["message"], "This action is unauthorized.");
}

#[test]
fn all_error_responses_include_request_id_field() {
    let _g = AppDebugGuard::falsy();

    // Outside a REQUEST_ID scope (which is the default for these unit
    // tests), `current_request_id()` returns None — which we render as
    // JSON null. The field is always present; the value is `null` when
    // unknown. This gives frontends a stable shape to parse.
    for err in [
        FrameworkError::internal("x"),
        FrameworkError::Domain {
            message: "y".into(),
            status_code: 400,
        },
        FrameworkError::Unauthorized,
        FrameworkError::param("z"),
    ] {
        let (_, body) = render(err);
        assert!(
            body.as_object()
                .map(|o| o.contains_key("request_id"))
                .unwrap_or(false),
            "every error body must contain `request_id` (null when absent) — got: {body}"
        );
    }
}

#[tokio::test]
async fn request_id_propagates_into_error_body_when_scope_active() {
    let _g = AppDebugGuard::falsy();

    let id = suprnova::logging::RequestId::from_string("rid-error-test-12345");
    let expected = id.as_str().to_string();

    let (_, body) = suprnova::logging::request_id::REQUEST_ID
        .scope(id, async { render(FrameworkError::internal("anything")) })
        .await;

    assert_eq!(
        body["request_id"], expected,
        "request_id from REQUEST_ID scope must land in the error body — got: {body}"
    );
}

#[test]
fn json_api_envelope_500_body_is_generic_in_production() {
    let _g = AppDebugGuard::falsy();

    let err = FrameworkError::internal("DB error: secret SQL fragment 'SELECT secret FROM users'");
    let resp = err.into_json_api_response();
    assert_eq!(resp.status_code(), 500);
    let body = resp.body().to_vec();
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let detail = parsed["errors"][0]["detail"]
        .as_str()
        .expect("detail is a string");
    assert_eq!(detail, "Internal server error");
    assert!(parsed["errors"][0]["meta"]["debug_message"].is_null());
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        !body_str.contains("SELECT"),
        "json:api envelope leaked raw err message: {body_str}"
    );
}

#[test]
fn json_api_envelope_500_with_debug_exposes_meta_debug_message() {
    let _g = AppDebugGuard::truthy();

    let raw = "DB error: secret SQL fragment 'SELECT secret FROM users'";
    let err = FrameworkError::internal(raw);
    let resp = err.into_json_api_response();
    let body = resp.body().to_vec();
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    let detail = parsed["errors"][0]["detail"]
        .as_str()
        .expect("detail is a string");
    // `detail` STAYS generic — same contract as the Laravel-shape body's
    // `message` field.
    assert_eq!(detail, "Internal server error");
    assert_eq!(
        parsed["errors"][0]["meta"]["debug_message"],
        format!("Internal server error: {raw}"),
        "debug_message should expose the full err.to_string() — got: {parsed}"
    );
}

#[test]
fn json_api_envelope_4xx_preserves_detail() {
    let _g = AppDebugGuard::falsy();

    let err = FrameworkError::Domain {
        message: "bad request: invalid type filter".into(),
        status_code: 400,
    };
    let resp = err.into_json_api_response();
    assert_eq!(resp.status_code(), 400);
    let body = resp.body().to_vec();
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        parsed["errors"][0]["detail"], "bad request: invalid type filter",
        "json:api 4xx must preserve detail — got: {parsed}"
    );
}
