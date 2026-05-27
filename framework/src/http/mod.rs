pub mod body;
pub mod cookie;
mod extract;
mod form_request;
mod request;
mod response;
pub mod upload;

pub use body::{collect_body, parse_form, parse_json};
pub use cookie::{Cookie, CookieOptions, SameSite, parse_cookies};
pub use extract::{FromParam, FromRequest};
pub use form_request::FormRequest;
pub use request::{BodyState, Request, RequestParts};
pub use response::{HttpResponse, Redirect, RedirectRouteBuilder, Response, ResponseExt};

/// Error type for missing route parameters
///
/// This type is kept for backward compatibility. New code should use
/// `FrameworkError::param()` instead.
#[derive(Debug)]
pub struct ParamError {
    pub param_name: String,
}

impl From<ParamError> for HttpResponse {
    fn from(err: ParamError) -> HttpResponse {
        // Route through `FrameworkError` so this legacy conversion produces
        // the same canonical `{ "message": ... }` body and 400 status as
        // every other error path, instead of the divergent `{ "error": ... }`
        // shape it used to emit.
        HttpResponse::from(crate::error::FrameworkError::from(err))
    }
}

impl From<ParamError> for crate::error::FrameworkError {
    fn from(err: ParamError) -> crate::error::FrameworkError {
        crate::error::FrameworkError::ParamError {
            param_name: err.param_name,
        }
    }
}

impl From<ParamError> for Response {
    fn from(err: ParamError) -> Response {
        Err(HttpResponse::from(crate::error::FrameworkError::from(err)))
    }
}

/// Create a text response
pub fn text(body: impl Into<String>) -> Response {
    Ok(HttpResponse::text(body))
}

/// Create a JSON response from a serde_json::Value
pub fn json(body: serde_json::Value) -> Response {
    Ok(HttpResponse::json(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The legacy `From<ParamError> for HttpResponse` must emit the same
    /// canonical `{ "message": ... }` body and 400 status as every other
    /// error path — not the old divergent `{ "error": ... }` shape.
    #[test]
    fn param_error_renders_canonical_message_shape() {
        let resp = HttpResponse::from(ParamError {
            param_name: "id".to_string(),
        });
        assert_eq!(resp.status_code(), 400);
        let body = std::str::from_utf8(resp.body()).expect("utf-8 body");
        assert!(
            body.contains("\"message\""),
            "body must use the canonical `message` key: {body}"
        );
        assert!(
            !body.contains("\"error\""),
            "the legacy `error` key must be gone: {body}"
        );
        assert!(
            body.contains("Missing required parameter: id"),
            "message text preserved: {body}"
        );
    }
}
