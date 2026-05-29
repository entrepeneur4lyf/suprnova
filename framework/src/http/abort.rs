//! Abort helpers — Laravel-style early-exit for handlers and middleware.
//!
//! Laravel's `abort($code, $message)` throws an `HttpException` to
//! short-circuit a controller. PHP has unwinding exceptions; Rust does
//! not. Suprnova's idiomatic equivalent returns a `FrameworkError`
//! whose `From<FrameworkError> for HttpResponse` impl renders the
//! standard `{ message, request_id }` JSON envelope at the right
//! status code. Use these helpers with the `?` operator:
//!
//! ```rust,ignore
//! use suprnova::{abort_if, Request, Response};
//!
//! pub async fn show(req: Request) -> Response {
//!     abort_if(req.param("id")? == "0", 404, "User not found")?;
//!     // ... handler continues only when condition is false ...
//!     Ok(json_response!({ "id": 1 }))
//! }
//! ```

use crate::error::FrameworkError;

/// Return an error at the given status. Mirrors Laravel's
/// `abort($code, $message)`. Use with `?`:
///
/// ```rust,ignore
/// abort(404, "User not found")?;
/// ```
///
/// `status` must be a real HTTP status code (100..=599). Values out of
/// range are coerced to 500 by the response renderer's status
/// validation (same behavior as
/// `HttpResponse::into_hyper`'s status fallback) — so callers don't
/// need to defend against bad input here.
pub fn abort(status: u16, message: impl Into<String>) -> Result<(), FrameworkError> {
    Err(FrameworkError::Domain {
        message: message.into(),
        status_code: status,
    })
}

/// Return an error when `condition` is true. Mirrors Laravel's
/// `abort_if($condition, $code, $message)`.
///
/// Returns `Ok(())` when the condition is false; the caller's `?`
/// then continues normally.
pub fn abort_if(
    condition: bool,
    status: u16,
    message: impl Into<String>,
) -> Result<(), FrameworkError> {
    if condition {
        abort(status, message)
    } else {
        Ok(())
    }
}

/// Return an error when `condition` is false. Mirrors Laravel's
/// `abort_unless($condition, $code, $message)`.
pub fn abort_unless(
    condition: bool,
    status: u16,
    message: impl Into<String>,
) -> Result<(), FrameworkError> {
    abort_if(!condition, status, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abort_returns_err_with_status_and_message() {
        let result = abort(404, "Not found");
        match result {
            Err(FrameworkError::Domain {
                status_code,
                message,
            }) => {
                assert_eq!(status_code, 404);
                assert_eq!(message, "Not found");
            }
            _ => panic!("abort must produce Err(FrameworkError::Domain {{...}})"),
        }
    }

    #[test]
    fn abort_if_true_returns_err() {
        let result = abort_if(true, 403, "Forbidden");
        assert!(result.is_err());
    }

    #[test]
    fn abort_if_false_returns_ok() {
        let result = abort_if(false, 403, "Forbidden");
        assert!(result.is_ok());
    }

    #[test]
    fn abort_unless_false_returns_err() {
        let result = abort_unless(false, 401, "Unauthorized");
        assert!(result.is_err());
    }

    #[test]
    fn abort_unless_true_returns_ok() {
        let result = abort_unless(true, 401, "Unauthorized");
        assert!(result.is_ok());
    }

    #[test]
    fn abort_renders_via_framework_error_to_response() {
        // The whole point of `abort` is that the resulting
        // `FrameworkError` walks through `From<FrameworkError> for
        // HttpResponse` and produces a proper status + body. Lock that
        // contract here.
        use crate::http::HttpResponse;
        let err = match abort(418, "I'm a teapot") {
            Err(e) => e,
            Ok(()) => panic!("unreachable"),
        };
        let resp: HttpResponse = err.into();
        assert_eq!(resp.status_code(), 418);
    }
}
