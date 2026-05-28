//! Framework-wide error types
//!
//! Provides a unified error type that can be used throughout the framework
//! and automatically converts to appropriate HTTP responses.

use std::collections::HashMap;
use thiserror::Error;

/// Trait for errors that can be converted to HTTP responses
///
/// Implement this trait on your domain errors to customize the HTTP status code
/// and message that will be returned when the error is converted to a response.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::HttpError;
///
/// #[derive(Debug)]
/// struct UserNotFoundError { user_id: i32 }
///
/// impl std::fmt::Display for UserNotFoundError {
///     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
///         write!(f, "User {} not found", self.user_id)
///     }
/// }
///
/// impl std::error::Error for UserNotFoundError {}
///
/// impl HttpError for UserNotFoundError {
///     fn status_code(&self) -> u16 { 404 }
/// }
/// ```
pub trait HttpError: std::error::Error + Send + Sync + 'static {
    /// HTTP status code (default: 500)
    fn status_code(&self) -> u16 {
        500
    }

    /// Error message for HTTP response (default: error's Display)
    fn error_message(&self) -> String {
        self.to_string()
    }
}

/// Simple wrapper for creating one-off domain errors
///
/// Use this for inline/ad-hoc errors when you don't want to create
/// a dedicated error type.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{AppError, FrameworkError};
///
/// pub async fn process() -> Result<(), FrameworkError> {
///     if invalid {
///         return Err(AppError::bad_request("Invalid input").into());
///     }
///     Ok(())
/// }
/// ```
#[derive(Debug, Clone)]
pub struct AppError {
    message: String,
    status_code: u16,
}

impl AppError {
    /// Create a new AppError with status 500 (Internal Server Error)
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            status_code: 500,
        }
    }

    /// Set the HTTP status code
    pub fn status(mut self, code: u16) -> Self {
        self.status_code = code;
        self
    }

    /// Create a 404 Not Found error
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(message).status(404)
    }

    /// Create a 400 Bad Request error
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(message).status(400)
    }

    /// Create a 401 Unauthorized error
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(message).status(401)
    }

    /// Create a 403 Forbidden error
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(message).status(403)
    }

    /// Create a 422 Unprocessable Entity error
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(message).status(422)
    }

    /// Create a 409 Conflict error
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(message).status(409)
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

impl HttpError for AppError {
    fn status_code(&self) -> u16 {
        self.status_code
    }

    fn error_message(&self) -> String {
        self.message.clone()
    }
}

impl From<AppError> for FrameworkError {
    fn from(e: AppError) -> Self {
        FrameworkError::Domain {
            message: e.message,
            status_code: e.status_code,
        }
    }
}

/// Validation errors with Laravel/Inertia-compatible format
///
/// Contains a map of field names to error messages, supporting multiple
/// errors per field.
///
/// # Response Format
///
/// When converted to an HTTP response, produces Laravel-compatible JSON:
///
/// ```json
/// {
///     "message": "The given data was invalid.",
///     "errors": {
///         "email": ["The email field must be a valid email address."],
///         "password": ["The password field must be at least 8 characters."]
///     }
/// }
/// ```
#[derive(Debug, Clone)]
pub struct ValidationErrors {
    /// Map of field names to their validation error messages
    pub errors: HashMap<String, Vec<String>>,
}

impl ValidationErrors {
    /// Create a new empty ValidationErrors
    pub fn new() -> Self {
        Self {
            errors: HashMap::new(),
        }
    }

    /// Add an error for a specific field
    pub fn add(&mut self, field: impl Into<String>, message: impl Into<String>) {
        self.errors
            .entry(field.into())
            .or_default()
            .push(message.into());
    }

    /// Add an error scoped under a named bag (Laravel's
    /// `withErrors($errors, 'profile')`). The scope name is prepended
    /// to the field key with a `.` separator, producing keys like
    /// `profile.bio` in the unified `errors` map.
    ///
    /// Use this when a single response carries errors from multiple
    /// forms or sub-forms that can't share a flat field namespace.
    pub fn add_to_bag(
        &mut self,
        bag: impl AsRef<str>,
        field: impl Into<String>,
        message: impl Into<String>,
    ) {
        let scoped = format!("{}.{}", bag.as_ref(), field.into());
        self.add(scoped, message);
    }

    /// Check if there are any errors
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Convert into a `Result`: `Ok(())` if the error bag is empty,
    /// `Err(self)` otherwise.
    ///
    /// Designed for the tail of an `after_validation` body (and the
    /// expansion of the [`crate::validate!`] macro):
    ///
    /// ```rust,ignore
    /// fn after_validation(&self) -> Result<(), ValidationErrors> {
    ///     let mut errs = ValidationErrors::new();
    ///     // ... accumulate via Rule::check / AsyncRule::check_async ...
    ///     errs.into_result()
    /// }
    /// ```
    pub fn into_result(self) -> Result<(), Self> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self)
        }
    }

    /// Convert from validator crate's ValidationErrors
    pub fn from_validator(errors: validator::ValidationErrors) -> Self {
        let mut result = Self::new();
        for (field, field_errors) in errors.field_errors() {
            for error in field_errors {
                let message = error
                    .message
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| format!("Validation failed for field '{}'", field));
                result.add(field.to_string(), message);
            }
        }
        result
    }

    /// Convert to JSON Value for response
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "message": "The given data was invalid.",
            "errors": self.errors
        })
    }

    /// Return a new `ValidationErrors` containing only the entries whose
    /// field name appears in `keep`. Used by Precognition's
    /// `Precognition-Validate-Only` header — the server runs full
    /// validation but reports errors only for the fields the client
    /// asked about.
    pub fn retain_fields(&self, keep: &[String]) -> Self {
        let kept = self
            .errors
            .iter()
            .filter(|(k, _)| keep.iter().any(|w| w == *k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Self { errors: kept }
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn retain_fields_keeps_only_listed() {
        let mut errs = ValidationErrors::new();
        errs.add("email", "invalid");
        errs.add("password", "too short");
        errs.add("name", "required");
        let kept = errs.retain_fields(&["email".to_string(), "name".to_string()]);
        assert!(kept.errors.contains_key("email"));
        assert!(kept.errors.contains_key("name"));
        assert!(!kept.errors.contains_key("password"));
    }

    #[test]
    fn retain_fields_empty_keep_returns_empty() {
        let mut errs = ValidationErrors::new();
        errs.add("email", "invalid");
        let kept = errs.retain_fields(&[]);
        assert!(kept.is_empty());
    }

    #[test]
    fn retain_fields_no_match_returns_empty() {
        let mut errs = ValidationErrors::new();
        errs.add("email", "invalid");
        let kept = errs.retain_fields(&["nonexistent".to_string()]);
        assert!(kept.is_empty());
    }

    #[test]
    fn precognition_success_status_204() {
        let e = FrameworkError::PrecognitionSuccess;
        assert_eq!(e.status_code(), 204);
    }

    #[test]
    fn precognition_failure_status_422() {
        let errs = ValidationErrors::new();
        let e = FrameworkError::PrecognitionFailure(errs);
        assert_eq!(e.status_code(), 422);
    }
}

impl Default for ValidationErrors {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Validation failed: {:?}", self.errors)
    }
}

impl std::error::Error for ValidationErrors {}

/// Framework-wide error type
///
/// This enum represents all possible errors that can occur in the framework.
/// It implements `From<FrameworkError> for Response` so errors can be propagated
/// using the `?` operator in controller handlers.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{App, FrameworkError, Response};
///
/// pub async fn index(_req: Request) -> Response {
///     let service = App::resolve::<MyService>()?;  // Returns FrameworkError on failure
///     // ...
/// }
/// ```
///
/// # Automatic Error Conversion
///
/// `FrameworkError` implements `From` for common error types, allowing seamless
/// use of the `?` operator:
///
/// ```rust,ignore
/// use suprnova::{DB, FrameworkError};
/// use sea_orm::ActiveModelTrait;
///
/// pub async fn create_todo() -> Result<Todo, FrameworkError> {
///     let todo = new_todo.insert(&*DB::get()?).await?;  // DbErr converts automatically!
///     Ok(todo)
/// }
/// ```
#[derive(Debug, Clone, Error)]
pub enum FrameworkError {
    /// Service not found in the dependency injection container
    #[error("Service '{type_name}' not registered in container")]
    ServiceNotFound {
        /// The type name of the service that was not found
        type_name: &'static str,
    },

    /// Parameter extraction failed (missing or invalid parameter)
    #[error("Missing required parameter: {param_name}")]
    ParamError {
        /// The name of the parameter that failed extraction
        param_name: String,
    },

    /// Validation error
    #[error("Validation error for '{field}': {message}")]
    ValidationError {
        /// The field that failed validation
        field: String,
        /// The validation error message
        message: String,
    },

    /// Database error
    #[error("Database error: {0}")]
    Database(String),

    /// Generic internal server error
    #[error("Internal server error: {message}")]
    Internal {
        /// The error message
        message: String,
    },

    /// Domain/application error with custom status code
    ///
    /// Used for user-defined domain errors that need custom HTTP status codes.
    #[error("{message}")]
    Domain {
        /// The error message
        message: String,
        /// HTTP status code
        status_code: u16,
    },

    /// Form validation errors (422 Unprocessable Entity)
    ///
    /// Contains multiple field validation errors in Laravel/Inertia format.
    #[error("Validation failed")]
    Validation(ValidationErrors),

    /// Authorization failed (403 Forbidden)
    ///
    /// Used when FormRequest::authorize() returns false.
    #[error("This action is unauthorized.")]
    Unauthorized,

    /// Model not found (404 Not Found)
    ///
    /// Used when route model binding fails to find the requested resource.
    #[error("{model_name} not found")]
    ModelNotFound {
        /// The name of the model that was not found
        model_name: String,
    },

    /// Parameter parse error (400 Bad Request)
    ///
    /// Used when a path parameter cannot be parsed to the expected type.
    #[error("Invalid parameter '{param}': expected {expected_type}")]
    ParamParse {
        /// The parameter value that failed to parse
        param: String,
        /// The expected type (e.g., "i32", "uuid")
        expected_type: &'static str,
    },

    /// Unsupported media type (415 Unsupported Media Type)
    ///
    /// Returned by `FormRequest::extract` when the request body's
    /// `Content-Type` is neither form-urlencoded nor a JSON media type
    /// (`application/json` or an `application/*+json` suffix) — including a
    /// missing or empty `Content-Type`. The framework refuses to guess at
    /// the body format rather than silently parsing an unknown body as JSON.
    #[error("Unsupported Media Type")]
    UnsupportedMediaType,

    /// Precognition validation passed (204 No Content)
    ///
    /// Returned by `FormRequest::extract` when the request carries a
    /// `Precognition: true` header and the (possibly field-filtered)
    /// validation passed. The controller body is skipped. The response
    /// converter emits 204 with `Precognition: true`,
    /// `Precognition-Success: true`, `Vary: Precognition`.
    #[error("Precognition validation passed")]
    PrecognitionSuccess,

    /// Precognition validation failed (422 Unprocessable Entity)
    ///
    /// Same shape as `Validation` but the response converter adds the
    /// `Precognition: true` + `Vary: Precognition` headers so the
    /// client (and any intermediary cache) sees the Precognition
    /// envelope.
    #[error("Precognition validation failed")]
    PrecognitionFailure(ValidationErrors),

    /// CLI sentinel: the failure has already been reported to the user
    /// (e.g. clap formatted and printed its own parse error). Callers
    /// translate this to a non-zero exit code without printing
    /// anything — see [`Self::silent`] / [`Self::is_silent`] for the
    /// pair used by the console dispatcher. Has no HTTP meaning;
    /// `status_code()` returns 500 only because the enum is
    /// HTTP-flavored.
    #[error("")]
    AlreadyReported,
}

impl FrameworkError {
    /// Create a ServiceNotFound error for a given type
    pub fn service_not_found<T: ?Sized>() -> Self {
        Self::ServiceNotFound {
            type_name: std::any::type_name::<T>(),
        }
    }

    /// Create a ParamError for a missing parameter
    pub fn param(name: impl Into<String>) -> Self {
        Self::ParamError {
            param_name: name.into(),
        }
    }

    /// Create a ValidationError
    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::ValidationError {
            field: field.into(),
            message: message.into(),
        }
    }

    /// Create a DatabaseError
    pub fn database(message: impl Into<String>) -> Self {
        Self::Database(message.into())
    }

    /// Create an Internal error
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }

    /// CLI sentinel: returns a [`Self::AlreadyReported`] variant signaling
    /// "the user has already seen the message." The console dispatcher
    /// uses this when clap's `try_get_matches_from` produces an error
    /// that clap formatted and printed itself — the binary's `main`
    /// translates this to a non-zero exit code without `eprintln`,
    /// avoiding a double-print.
    ///
    /// Pair with [`Self::is_silent`] at the consume site. Type-safe:
    /// constructing `FrameworkError::internal("")` directly does NOT
    /// produce a silent error — only this constructor does.
    pub fn silent() -> Self {
        Self::AlreadyReported
    }

    /// Whether this error has already been reported to the user.
    /// See [`Self::silent`] for the producer side. The console
    /// dispatcher checks this before emitting its own `eprintln`
    /// for handler-returned errors, so users never see two error
    /// messages for the same failure.
    pub fn is_silent(&self) -> bool {
        matches!(self, Self::AlreadyReported)
    }

    /// Create a Domain error with custom status code
    pub fn domain(message: impl Into<String>, status_code: u16) -> Self {
        Self::Domain {
            message: message.into(),
            status_code,
        }
    }

    /// Create a generic bad-request (400) error.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::Domain {
            message: message.into(),
            status_code: 400,
        }
    }

    /// Get the HTTP status code for this error
    pub fn status_code(&self) -> u16 {
        match self {
            Self::ServiceNotFound { .. } => 500,
            Self::ParamError { .. } => 400,
            Self::ValidationError { .. } => 422,
            Self::Database(_) => 500,
            Self::Internal { .. } => 500,
            Self::Domain { status_code, .. } => *status_code,
            Self::Validation(_) => 422,
            Self::Unauthorized => 403,
            Self::ModelNotFound { .. } => 404,
            Self::ParamParse { .. } => 400,
            Self::UnsupportedMediaType => 415,
            Self::PrecognitionSuccess => 204,
            Self::PrecognitionFailure(_) => 422,
            Self::AlreadyReported => 500,
        }
    }

    /// Create a Validation error from ValidationErrors struct
    pub fn validation_errors(errors: ValidationErrors) -> Self {
        Self::Validation(errors)
    }

    /// Turn a database write error into a field-scoped 422 validation
    /// error **when** it is a unique-constraint violation; otherwise pass
    /// the original error through unchanged (a 500-class `Database`
    /// error).
    ///
    /// This closes the gap left by the [`Unique`] validation rule.
    /// `Unique` runs a `SELECT COUNT(*)` *before* the write, so it is an
    /// **advisory** check with an unavoidable time-of-check/time-of-use
    /// race: two concurrent requests can both pass the pre-check and then
    /// both attempt the insert. The only real guarantee is a `UNIQUE`
    /// constraint (or unique index) on the column in the database. This
    /// helper lets the handler catch the constraint violation the loser
    /// of that race receives and render it as the same clean 422 the
    /// advisory rule would have produced, instead of leaking a 500:
    ///
    /// ```rust,ignore
    /// // `users.email` has a UNIQUE constraint in the migration.
    /// let user = new_user
    ///     .insert(db)
    ///     .await
    ///     .map_err(|e| FrameworkError::from_unique_violation(
    ///         "email",
    ///         "That email address is already registered.",
    ///         e,
    ///     ))?;
    /// ```
    ///
    /// Use the advisory [`Unique`] rule for a friendly pre-submit message
    /// (and Precognition), and this helper at the write site for the
    /// authoritative answer. Backend coverage is whatever SeaORM's
    /// [`DbErr::sql_err`] recognises — MySQL, Postgres, and SQLite all
    /// map their duplicate-key errors to
    /// [`SqlErr::UniqueConstraintViolation`].
    ///
    /// [`Unique`]: crate::validation::rule::async_rules::Unique
    /// [`DbErr::sql_err`]: sea_orm::DbErr::sql_err
    /// [`SqlErr::UniqueConstraintViolation`]: sea_orm::SqlErr::UniqueConstraintViolation
    pub fn from_unique_violation(
        field: impl Into<String>,
        message: impl Into<String>,
        err: sea_orm::DbErr,
    ) -> Self {
        match err.sql_err() {
            Some(sea_orm::SqlErr::UniqueConstraintViolation(_)) => {
                let mut errors = ValidationErrors::new();
                errors.add(field, message);
                Self::Validation(errors)
            }
            _ => err.into(),
        }
    }

    /// Create a ModelNotFound error (404)
    pub fn model_not_found(name: impl Into<String>) -> Self {
        Self::ModelNotFound {
            model_name: name.into(),
        }
    }

    /// Create a ParamParse error (400)
    pub fn param_parse(param: impl Into<String>, expected_type: &'static str) -> Self {
        Self::ParamParse {
            param: param.into(),
            expected_type,
        }
    }

    /// Create a 404 Not Found error (convenience constructor).
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::ModelNotFound {
            model_name: message.into(),
        }
    }

    /// Return the primary human-readable error message. Used by
    /// `into_json_api_response` to populate the `detail` field.
    pub fn message(&self) -> &str {
        match self {
            Self::ServiceNotFound { type_name } => type_name,
            Self::ParamError { param_name } => param_name,
            Self::ValidationError { message, .. } => message,
            Self::Database(msg) => msg,
            Self::Internal { message } => message,
            Self::Domain { message, .. } => message,
            Self::Validation(_) => "Validation failed",
            Self::Unauthorized => "This action is unauthorized.",
            Self::ModelNotFound { model_name } => model_name,
            Self::ParamParse { param, .. } => param,
            Self::UnsupportedMediaType => "Unsupported Media Type",
            Self::PrecognitionSuccess => "Precognition validation passed",
            Self::PrecognitionFailure(_) => "Precognition validation failed",
            Self::AlreadyReported => "",
        }
    }

    /// Return the field associated with this error, if any. Used by
    /// `into_json_api_response` to populate `source.pointer`.
    /// Only `ValidationError` carries a field name.
    pub fn field(&self) -> Option<&str> {
        match self {
            Self::ValidationError { field, .. } => Some(field),
            _ => None,
        }
    }

    /// Wrap this error with a context string. The status code is
    /// preserved; the display becomes `"<ctx>: <original>"`.
    ///
    /// Use this when an error needs to be re-raised with operation
    /// context:
    ///
    /// ```ignore
    /// db.insert(user).await
    ///     .map_err(FrameworkError::from)
    ///     .map_err(|e| e.context("creating new user"))?;
    /// ```
    pub fn context(self, ctx: impl Into<String>) -> Self {
        let prefix = ctx.into();
        let status = self.status_code();
        let original = self.to_string();
        Self::Domain {
            message: format!("{}: {}", prefix, original),
            status_code: status,
        }
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn context_prepends_to_message_preserving_status() {
        let inner = FrameworkError::internal("disk full");
        assert_eq!(inner.status_code(), 500);

        let wrapped = inner.context("writing user avatar");
        assert!(wrapped.to_string().contains("writing user avatar"));
        assert!(wrapped.to_string().contains("disk full"));
        assert_eq!(wrapped.status_code(), 500);
    }

    #[test]
    fn context_preserves_non_500_status_codes() {
        let inner = FrameworkError::param("user_id");
        assert_eq!(inner.status_code(), 400);
        let wrapped = inner.context("decoding request");
        assert_eq!(wrapped.status_code(), 400);
        assert!(wrapped.to_string().contains("decoding request"));
    }

    #[test]
    fn context_chains_multiple_layers() {
        let err = FrameworkError::internal("io error")
            .context("reading config")
            .context("loading service");
        let msg = err.to_string();
        assert!(msg.contains("loading service"));
        assert!(msg.contains("reading config"));
        assert!(msg.contains("io error"));
    }
}

// Implement From<DbErr> for automatic error conversion with ?
impl From<sea_orm::DbErr> for FrameworkError {
    fn from(e: sea_orm::DbErr) -> Self {
        Self::Database(e.to_string())
    }
}

// Implement From<opendal::Error> so storage operations propagate through `?`
// in handler/service code that already returns `FrameworkError`.
impl From<opendal::Error> for FrameworkError {
    fn from(e: opendal::Error) -> Self {
        Self::Internal {
            message: format!("storage: {e}"),
        }
    }
}
