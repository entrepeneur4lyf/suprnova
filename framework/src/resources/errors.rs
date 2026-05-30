//! JSON:API error envelope for `FrameworkError`.

use crate::error::FrameworkError;
use crate::http::HttpResponse;
use bytes::Bytes;
use serde_json::{Value, json};

impl FrameworkError {
    /// Render a JSON:API `{"errors": [...]}` response envelope.
    ///
    /// Sets `Content-Type: application/vnd.api+json` and the appropriate
    /// HTTP status code. The `source.pointer` field is set when the error
    /// carries a field name (i.e. `ValidationError`).
    ///
    /// 5xx sanitisation: for status >= 500 `detail` is replaced with the
    /// generic title; the raw err message is never returned to clients.
    /// When `APP_DEBUG=true` (false by default outside local/dev/test) a
    /// `meta.debug_message` field is added for development visibility.
    /// `request_id` is exposed under `meta` on every error so frontends
    /// and operators can correlate the client response to the
    /// structured log entry.
    ///
    /// 4xx detail uses the full `Display` string (matching the normal
    /// JSON error renderer in `http::response`) — e.g. `"Invalid
    /// parameter 'id': expected uuid"` rather than the bare param name.
    /// The `Error::message()` accessor returns only the per-variant
    /// payload (param name, model name, etc.) and is suitable for
    /// internal pointer/source extraction, not for caller-facing
    /// `detail`.
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
        // for the debug-only meta.debug_message field below; for 4xx
        // it also becomes the user-facing `detail` value so JSON:API
        // clients see the same richer message the regular JSON error
        // renderer emits — except for `ValidationError`, where the
        // envelope already carries the field name in `source.pointer`,
        // so the bare message stays in `detail` rather than the
        // doubled "Validation error for 'email': email is invalid".
        let full_message = self.to_string();
        let detail = if status >= 500 {
            // Generic detail — never the raw err message.
            title.to_string()
        } else if matches!(self, FrameworkError::ValidationError { .. }) {
            // Field name is exposed under source.pointer; detail
            // carries only the message to avoid redundancy.
            self.message().to_string()
        } else {
            full_message.clone()
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
        // Gated through `Config::is_debug` so a programmatically-
        // registered `AppConfig` (e.g. forced fail-closed for a staging
        // environment) wins over the env-derived default. Falls back to
        // env-derived AppConfig if the repository hasn't been seeded
        // yet — which is fail-closed in production-shaped envs.
        if status >= 500 && crate::config::Config::is_debug() {
            meta.insert("debug_message".into(), Value::String(full_message));
        }
        err_obj.insert("meta".into(), Value::Object(meta));

        let body = json!({ "errors": [Value::Object(err_obj)] });
        let bytes = serde_json::to_vec(&body).expect("JSON:API error encode infallible");
        HttpResponse::bytes_body(Bytes::from(bytes), "application/vnd.api+json").status(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_json(resp: &HttpResponse) -> serde_json::Value {
        let bytes = resp.body();
        assert!(!bytes.is_empty(), "JSON:API error body should be static");
        serde_json::from_slice(bytes).expect("JSON:API body is valid JSON")
    }

    #[test]
    fn json_api_detail_uses_full_display_for_param_parse() {
        let err = FrameworkError::param_parse("id", "uuid");
        let expected = err.to_string();
        let resp = err.into_json_api_response();
        assert_eq!(resp.status_code(), 400);
        let body = body_json(&resp);
        let detail = body["errors"][0]["detail"]
            .as_str()
            .expect("detail is a string");
        assert_eq!(detail, expected);
        assert!(detail.contains("Invalid parameter"));
        assert!(detail.contains("uuid"));
    }

    #[test]
    fn json_api_detail_uses_full_display_for_model_not_found() {
        let err = FrameworkError::model_not_found("User");
        let expected = err.to_string();
        let resp = err.into_json_api_response();
        assert_eq!(resp.status_code(), 404);
        let body = body_json(&resp);
        let detail = body["errors"][0]["detail"]
            .as_str()
            .expect("detail is a string");
        assert_eq!(detail, expected);
        assert_eq!(detail, "User not found");
    }

    #[test]
    fn json_api_detail_uses_full_display_for_param_error() {
        let err = FrameworkError::param("user_id");
        let expected = err.to_string();
        let resp = err.into_json_api_response();
        assert_eq!(resp.status_code(), 400);
        let body = body_json(&resp);
        let detail = body["errors"][0]["detail"]
            .as_str()
            .expect("detail is a string");
        assert_eq!(detail, expected);
        assert!(detail.contains("Missing required parameter"));
    }

    #[test]
    fn json_api_500_detail_stays_generic() {
        let err = FrameworkError::internal("internal pii: db=foo password=bar");
        let resp = err.into_json_api_response();
        assert_eq!(resp.status_code(), 500);
        let body = body_json(&resp);
        let detail = body["errors"][0]["detail"]
            .as_str()
            .expect("detail is a string");
        assert_eq!(detail, "Internal server error");
        assert!(!detail.contains("password"));
        assert!(!detail.contains("pii"));
    }
}
