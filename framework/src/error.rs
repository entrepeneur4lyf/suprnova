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

    /// Check if there are any errors
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
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

    /// Create a Domain error with custom status code
    pub fn domain(message: impl Into<String>, status_code: u16) -> Self {
        Self::Domain {
            message: message.into(),
            status_code,
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
            Self::PrecognitionSuccess => 204,
            Self::PrecognitionFailure(_) => 422,
        }
    }

    /// Create a Validation error from ValidationErrors struct
    pub fn validation_errors(errors: ValidationErrors) -> Self {
        Self::Validation(errors)
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
}

// Implement From<DbErr> for automatic error conversion with ?
impl From<sea_orm::DbErr> for FrameworkError {
    fn from(e: sea_orm::DbErr) -> Self {
        Self::Database(e.to_string())
    }
}
