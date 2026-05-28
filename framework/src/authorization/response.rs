//! Rich authorization decision type.

use crate::FrameworkError;

/// A rich authorization decision, mirroring Laravel's
/// `Illuminate\Auth\Access\Response`.
///
/// A bare `bool` gate answers only allow/deny. A `Response` additionally
/// carries a human-readable denial *message*, an optional machine *code*, and
/// an optional HTTP *status* so a denial can surface as a 404 / 422 / 403
/// rather than the uniform 403.
///
/// Gates registered with [`Gate::define`](crate::Gate::define) return `bool`
/// and are wrapped into a bare allow/deny `Response`. Gates registered with
/// [`Gate::define_with`](crate::Gate::define_with) return a `Response`
/// directly — that is how a custom denial message reaches
/// [`Gate::inspect`](crate::Gate::inspect).
///
/// # Naming
///
/// The crate root already binds `Response` to the HTTP response contract
/// (`Result<HttpResponse, HttpResponse>`), so this type is **not** exported
/// there as bare `Response`. It lives at `suprnova::authorization::Response`
/// and is re-exported at the crate root as `GateResponse`. Import it under the
/// Laravel spelling when you want it:
///
/// ```ignore
/// use suprnova::authorization::Response;
///
/// Gate::define_with::<User, Post>("update", |u, p| {
///     if p.author_id == u.id {
///         Response::allow()
///     } else {
///         Response::deny_with("You do not own this post.")
///     }
/// });
/// ```
///
/// # Serialization
///
/// `Response` serializes to Laravel's `toArray()` shape —
/// `{ "allowed", "message", "code" }`. The HTTP `status` is intentionally
/// **not** serialized (it is a server-side routing concern, not part of the
/// decision the frontend consumes), matching Laravel.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Response {
    allowed: bool,
    message: Option<String>,
    code: Option<String>,
    /// HTTP status to apply when this denial is converted to an error. Not
    /// part of the serialized decision (mirrors Laravel's `toArray`).
    #[serde(skip)]
    status: Option<u16>,
}

impl Response {
    // ── Constructors ────────────────────────────────────────────────────────

    /// An allowing decision with no message.
    pub fn allow() -> Self {
        Self {
            allowed: true,
            message: None,
            code: None,
            status: None,
        }
    }

    /// A bare denial — no message, no status. Through
    /// [`authorize`](Self::authorize) this maps to the canonical
    /// `FrameworkError::Unauthorized` (403).
    pub fn deny() -> Self {
        Self {
            allowed: false,
            message: None,
            code: None,
            status: None,
        }
    }

    /// A denial carrying a human-readable message. The message survives
    /// through [`authorize`](Self::authorize) as the error body.
    pub fn deny_with(message: impl Into<String>) -> Self {
        Self {
            allowed: false,
            message: Some(message.into()),
            code: None,
            status: None,
        }
    }

    /// A denial carrying both a custom HTTP status and a message. Mirrors
    /// Laravel's `Response::denyWithStatus($status, $message)`.
    pub fn deny_with_status(status: u16, message: impl Into<String>) -> Self {
        Self {
            allowed: false,
            message: Some(message.into()),
            code: None,
            status: Some(status),
        }
    }

    /// A denial that surfaces as `404 Not Found` rather than `403`. Mirrors
    /// Laravel's `Response::denyAsNotFound()` — used to hide a resource's
    /// existence from a user who may not view it.
    pub fn deny_as_not_found() -> Self {
        Self {
            allowed: false,
            message: None,
            code: None,
            status: Some(404),
        }
    }

    // ── Builders ────────────────────────────────────────────────────────────

    /// Attach (or replace) the message.
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Attach (or replace) the machine-readable reason code.
    ///
    /// Note: `code` is reachable via [`code`](Self::code) on the inspected
    /// `Response` but does **not** round-trip through
    /// [`authorize`](Self::authorize) — `FrameworkError` has no code field.
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    /// Set the HTTP status applied when this denial becomes an error.
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// Set the HTTP status to `404`. Mirrors Laravel's `asNotFound()`.
    pub fn as_not_found(mut self) -> Self {
        self.status = Some(404);
        self
    }

    // ── Accessors ───────────────────────────────────────────────────────────

    /// Whether the decision allows the action.
    pub fn allowed(&self) -> bool {
        self.allowed
    }

    /// Whether the decision denies the action.
    pub fn denied(&self) -> bool {
        !self.allowed
    }

    /// The denial (or allowance) message, if any.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// The machine-readable reason code, if any.
    ///
    /// Available on the inspected `Response` only — it does not survive
    /// [`authorize`](Self::authorize) (`FrameworkError` carries no code).
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// The HTTP status to apply on denial, if set.
    pub fn status(&self) -> Option<u16> {
        self.status
    }

    // ── Conversion ──────────────────────────────────────────────────────────

    /// Collapse the rich decision into a `Result`.
    ///
    /// - Allowed → `Ok(self)` (so the response can be chained, as in Laravel).
    /// - Bare denial (no message / code / status) → `FrameworkError::Unauthorized`
    ///   (403, `"This action is unauthorized."`) — the canonical denial.
    /// - Rich denial → `FrameworkError::Domain { message, status_code }` carrying
    ///   the custom message (or the default) and the custom status (or 403).
    ///
    /// `code` is **not** represented in the resulting error — it has no field
    /// on `FrameworkError`. Inspect the `Response` directly if you need it.
    pub fn authorize(self) -> Result<Response, FrameworkError> {
        if self.allowed {
            Ok(self)
        } else if self.message.is_none() && self.code.is_none() && self.status.is_none() {
            Err(FrameworkError::Unauthorized)
        } else {
            Err(FrameworkError::Domain {
                message: self
                    .message
                    .clone()
                    .unwrap_or_else(|| "This action is unauthorized.".to_string()),
                status_code: self.status.unwrap_or(403),
            })
        }
    }
}

impl From<bool> for Response {
    fn from(allowed: bool) -> Self {
        if allowed { Self::allow() } else { Self::deny() }
    }
}

impl std::fmt::Display for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message.as_deref().unwrap_or(""))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_and_deny_basics() {
        let a = Response::allow();
        assert!(a.allowed() && !a.denied());
        assert_eq!(a.message(), None);
        assert_eq!(a.status(), None);

        let d = Response::deny();
        assert!(d.denied() && !d.allowed());
    }

    #[test]
    fn deny_with_carries_message() {
        let d = Response::deny_with("nope");
        assert_eq!(d.message(), Some("nope"));
        assert!(d.denied());
    }

    #[test]
    fn deny_with_status_and_not_found() {
        assert_eq!(Response::deny_with_status(422, "bad").status(), Some(422));
        assert_eq!(Response::deny_as_not_found().status(), Some(404));
        assert_eq!(Response::deny().as_not_found().status(), Some(404));
    }

    #[test]
    fn from_bool() {
        assert!(Response::from(true).allowed());
        assert!(Response::from(false).denied());
    }

    #[test]
    fn authorize_allow_is_ok() {
        assert!(Response::allow().authorize().is_ok());
    }

    #[test]
    fn authorize_bare_deny_is_unauthorized() {
        // A bare denial maps to the canonical 403 Unauthorized — preserving
        // the pre-Response behaviour of bool gates.
        assert!(matches!(
            Response::deny().authorize(),
            Err(FrameworkError::Unauthorized)
        ));
    }

    #[test]
    fn authorize_rich_deny_is_domain_with_status() {
        // deny_as_not_found → Domain { status_code: 404 }
        match Response::deny_as_not_found().authorize() {
            Err(FrameworkError::Domain {
                status_code: 404, ..
            }) => {}
            other => panic!("expected Domain 404, got {other:?}"),
        }
        // deny_with(message) but no status → Domain { 403, message }
        match Response::deny_with("you must own this").authorize() {
            Err(FrameworkError::Domain {
                message,
                status_code: 403,
            }) => assert_eq!(message, "you must own this"),
            other => panic!("expected Domain 403 with message, got {other:?}"),
        }
    }

    #[test]
    fn serialize_matches_laravel_to_array_shape() {
        let json =
            serde_json::to_value(Response::deny_with("x").with_code("E1").as_not_found()).unwrap();
        // status is intentionally absent (server-side concern).
        assert_eq!(json["allowed"], serde_json::json!(false));
        assert_eq!(json["message"], serde_json::json!("x"));
        assert_eq!(json["code"], serde_json::json!("E1"));
        assert!(json.get("status").is_none());
    }
}
