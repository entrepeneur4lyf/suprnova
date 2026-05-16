//! JSON:API error envelope for `FrameworkError`.

use bytes::Bytes;
use serde_json::{json, Value};
use crate::error::FrameworkError;
use crate::http::HttpResponse;

impl FrameworkError {
    /// Render a JSON:API `{"errors": [...]}` response envelope.
    ///
    /// Sets `Content-Type: application/vnd.api+json` and the appropriate
    /// HTTP status code. The `source.pointer` field is set when the error
    /// carries a field name (i.e. `ValidationError`).
    ///
    /// 5xx sanitisation (codex review finding #2): for status >= 500
    /// `detail` is replaced with the generic title; the raw err message
    /// is never returned to clients. When `APP_DEBUG=true` (false by
    /// default outside local/dev/test) a `meta.debug_message` field is
    /// added for development visibility. `request_id` is exposed under
    /// `meta` on every error so frontends and operators can correlate
    /// the client response to the structured log entry.
    pub fn into_json_api_response(self) -> HttpResponse {
        let status = self.status_code();
        let title = match status {
            422 => "Validation failed",
            404 => "Not found",
            403 => "Forbidden",
            401 => "Unauthorized",
            400 => "Bad request",
            500 => "Internal server error",
            _ => "Error",
        };
        // Full Display (e.g. "Internal server error: <inner>"). Stored
        // for the debug-only meta.debug_message field below; never
        // exposed in `detail` for 5xx responses.
        let full_message = self.to_string();
        let detail = if status >= 500 {
            // Generic detail — never the raw err message.
            title.to_string()
        } else {
            self.message().to_string()
        };

        let mut err_obj = serde_json::Map::new();
        err_obj.insert("status".into(), Value::String(status.to_string()));
        err_obj.insert("title".into(), Value::String(title.to_string()));
        err_obj.insert("detail".into(), Value::String(detail));
        if let Some(field) = self.field() {
            err_obj.insert(
                "source".into(),
                json!({ "pointer": format!("/data/attributes/{field}") }),
            );
        }

        // Meta block: request_id + (debug-only) full err message. Mirrors
        // the Laravel-shape body (HTTP/response.rs): the `debug_message`
        // field carries `err.to_string()`, while the user-facing
        // `detail` (analogous to Laravel `message`) stays generic for
        // 5xx so frontends never accidentally key on internal text.
        let request_id = crate::logging::current_request_id().map(|id| id.as_str().to_string());
        let mut meta = serde_json::Map::new();
        meta.insert(
            "request_id".into(),
            match &request_id {
                Some(id) => Value::String(id.clone()),
                None => Value::Null,
            },
        );
        if status >= 500 && crate::config::AppConfig::from_env().is_debug() {
            meta.insert("debug_message".into(), Value::String(full_message));
        }
        err_obj.insert("meta".into(), Value::Object(meta));

        let body = json!({ "errors": [Value::Object(err_obj)] });
        let bytes = serde_json::to_vec(&body).expect("JSON:API error encode infallible");
        HttpResponse::bytes_body(Bytes::from(bytes), "application/vnd.api+json")
            .status(status)
    }
}
