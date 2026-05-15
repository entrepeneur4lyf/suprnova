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
        let mut err_obj = serde_json::Map::new();
        err_obj.insert("status".into(), Value::String(status.to_string()));
        err_obj.insert("title".into(), Value::String(title.to_string()));
        err_obj.insert("detail".into(), Value::String(self.message().to_string()));
        if let Some(field) = self.field() {
            err_obj.insert(
                "source".into(),
                json!({ "pointer": format!("/data/attributes/{field}") }),
            );
        }
        let body = json!({ "errors": [Value::Object(err_obj)] });
        let bytes = serde_json::to_vec(&body).expect("JSON:API error encode infallible");
        HttpResponse::bytes_body(Bytes::from(bytes), "application/vnd.api+json")
            .status(status)
    }
}
