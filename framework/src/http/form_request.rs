//! FormRequest trait for validated request data
//!
//! Provides Laravel-like FormRequest pattern with automatic body parsing,
//! validation, and authorization.

use super::body::{parse_form, parse_json};
use super::extract::FromRequest;
use super::Request;
use crate::error::{FrameworkError, ValidationErrors};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use validator::Validate;

/// Trait for validated form/JSON request data
///
/// Implement this trait on request structs to enable automatic:
/// - Body parsing (JSON or form-urlencoded based on Content-Type)
/// - Validation using the `validator` crate
/// - Authorization checks
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::FormRequest;
/// use serde::Deserialize;
/// use validator::Validate;
///
/// #[derive(FormRequest)]  // Auto-derives Deserialize, Validate, and FormRequest impl
/// pub struct CreateUserRequest {
///     #[validate(email)]
///     pub email: String,
///
///     #[validate(length(min = 8))]
///     pub password: String,
/// }
///
/// // In controller:
/// #[handler]
/// pub async fn store(form: CreateUserRequest) -> Response {
///     // `form` is already validated - returns 422 if invalid
///     json_response!({ "email": form.email })
/// }
/// ```
///
/// # Authorization
///
/// Override `authorize()` to add authorization logic:
///
/// ```rust,ignore
/// impl FormRequest for CreateUserRequest {
///     fn authorize(_req: &Request) -> bool {
///         // Check if user is authenticated
///         true
///     }
/// }
/// ```
#[async_trait]
pub trait FormRequest: Sized + DeserializeOwned + Validate + Send {
    /// Check if the request is authorized
    ///
    /// Override this method to add authorization logic.
    /// Returns `true` by default (all requests authorized).
    ///
    /// Returning `false` will result in a 403 Forbidden response.
    fn authorize(_req: &Request) -> bool {
        true
    }

    /// Extract and validate data from the request
    ///
    /// This method:
    /// 1. Checks authorization
    /// 2. Parses the request body (JSON or form based on Content-Type)
    /// 3. Validates the parsed data
    ///
    /// Returns `Err(FrameworkError)` on authorization failure, parse error,
    /// or validation failure.
    async fn extract(req: Request) -> Result<Self, FrameworkError> {
        // Check authorization first
        if !Self::authorize(&req) {
            return Err(FrameworkError::Unauthorized);
        }

        // Detect Precognition envelope BEFORE consuming the body so
        // we know how to short-circuit. The client sends:
        //   Precognition: true                       — opt into the protocol
        //   Precognition-Validate-Only: a,b,c        — filter errors to these fields
        let is_precognition = req
            .header("Precognition")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let validate_only: Vec<String> = req
            .header("Precognition-Validate-Only")
            .map(|raw| {
                raw.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();

        // Get content type before consuming body
        let content_type = req.content_type().map(|s| s.to_string());

        // Collect and parse body
        let (_, bytes) = req.body_bytes().await?;

        let data: Self = match content_type.as_deref() {
            Some(ct) if ct.starts_with("application/x-www-form-urlencoded") => parse_form(&bytes)?,
            _ => parse_json(&bytes)?,
        };

        // Run validation. Precognition runs the same validators as a
        // real submission — we just decide what to do with the result.
        let validation_result = data.validate();

        if is_precognition {
            return match validation_result {
                Ok(()) => Err(FrameworkError::PrecognitionSuccess),
                Err(errors) => {
                    let errs = ValidationErrors::from_validator(errors);
                    let filtered = if validate_only.is_empty() {
                        // No filter — return all errors as the failure.
                        errs
                    } else {
                        errs.retain_fields(&validate_only)
                    };
                    if filtered.is_empty() {
                        // All real errors were on fields the client
                        // didn't ask about → success for what was asked.
                        Err(FrameworkError::PrecognitionSuccess)
                    } else {
                        Err(FrameworkError::PrecognitionFailure(filtered))
                    }
                }
            };
        }

        // Non-Precognition: standard flow.
        if let Err(errors) = validation_result {
            return Err(FrameworkError::Validation(
                ValidationErrors::from_validator(errors),
            ));
        }

        Ok(data)
    }
}

/// Blanket implementation of FromRequest for all FormRequest types
#[async_trait]
impl<T: FormRequest> FromRequest for T {
    async fn from_request(req: Request) -> Result<Self, FrameworkError> {
        T::extract(req).await
    }
}
