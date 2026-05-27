//! FormRequest trait for validated request data
//!
//! Provides Laravel-like FormRequest pattern with automatic body parsing,
//! validation, and authorization.

use super::Request;
use super::body::{parse_form, parse_json};
use super::extract::FromRequest;
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

    /// Cross-field validation hook. Called AFTER the derived
    /// `Validate` rules pass. Return `Err(ValidationErrors)` to
    /// surface additional errors (e.g. "passwords must match",
    /// "end_date must be after start_date").
    ///
    /// The default implementation returns `Ok(())`.
    ///
    /// This hook runs in both normal and Precognition flows. In
    /// Precognition mode the errors are filtered by the
    /// `Precognition-Validate-Only` header just like single-field
    /// validator errors, and surface as `FrameworkError::PrecognitionFailure`.
    /// In the standard flow they surface as `FrameworkError::Validation`
    /// (HTTP 422).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// impl FormRequest for UpdatePasswordRequest {
    ///     fn after_validation(&self) -> Result<(), ValidationErrors> {
    ///         if self.new_password != self.confirmation {
    ///             let mut errs = ValidationErrors::new();
    ///             errs.add("confirmation", "passwords do not match");
    ///             return Err(errs);
    ///         }
    ///         Ok(())
    ///     }
    /// }
    /// ```
    fn after_validation(&self) -> Result<(), ValidationErrors> {
        Ok(())
    }

    /// Maximum request body size (in bytes) accepted by this FormRequest.
    ///
    /// Defaults to the process-global cap
    /// ([`crate::http::body::global_max_request_body_bytes`]), which is
    /// itself derived from
    /// [`crate::http::body::DEFAULT_MAX_REQUEST_BODY_BYTES`] (8 MiB) unless
    /// the application has called
    /// [`crate::http::body::set_global_max_request_body_bytes`] at boot.
    ///
    /// Override this for endpoints that accept legitimately large JSON
    /// payloads (analytics ingest, bulk import, etc.):
    ///
    /// ```rust,ignore
    /// impl FormRequest for ImportPayload {
    ///     fn max_body_bytes() -> usize { 64 * 1024 * 1024 } // 64 MiB
    /// }
    /// ```
    ///
    /// Lower it for endpoints that should never receive large bodies
    /// (login, search query strings, etc.) to fail fast on abuse.
    fn max_body_bytes() -> usize {
        crate::http::body::global_max_request_body_bytes()
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

        // Get the content type before consuming the body and decide how to
        // parse from it. Strip any parameters (`; charset=...`), trim, and
        // lowercase so `Application/JSON; charset=utf-8` classifies the same
        // as `application/json`.
        let media_type = req.content_type().map(|ct| {
            ct.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        });

        // Only two body shapes are understood: form-urlencoded, and JSON
        // (`application/json` or any `application/*+json` suffix type). Every
        // other content type — including a missing or empty `Content-Type` —
        // is rejected with 415 rather than silently parsed as JSON. The check
        // runs BEFORE the body is read so an unsupported request never streams.
        let is_form = media_type.as_deref() == Some("application/x-www-form-urlencoded");
        let is_json = media_type
            .as_deref()
            .is_some_and(|mt| mt == "application/json" || mt.ends_with("+json"));
        if !is_form && !is_json {
            return Err(FrameworkError::UnsupportedMediaType);
        }

        // Collect and parse body. Honor the per-struct cap; `body_bytes_with_cap`
        // reads `Content-Length` from headers and pre-rejects oversized
        // requests with 413 before consuming any body bytes.
        let (_, bytes) = req.body_bytes_with_cap(Self::max_body_bytes()).await?;

        let data: Self = if is_form {
            parse_form(&bytes)?
        } else {
            parse_json(&bytes)?
        };

        // Run validation. Precognition runs the same validators as a
        // real submission — we just decide what to do with the result.
        let validation_result = data.validate();

        if is_precognition {
            return match validation_result {
                Ok(()) => {
                    // Per-field rules passed. Cross-field hook still
                    // has to run — a "passwords must match" failure
                    // should reach the Precognition client too.
                    match data.after_validation() {
                        Ok(()) => Err(FrameworkError::PrecognitionSuccess),
                        Err(errs) => {
                            let filtered = if validate_only.is_empty() {
                                errs
                            } else {
                                errs.retain_fields(&validate_only)
                            };
                            if filtered.is_empty() {
                                Err(FrameworkError::PrecognitionSuccess)
                            } else {
                                Err(FrameworkError::PrecognitionFailure(filtered))
                            }
                        }
                    }
                }
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

        // Per-field rules passed — run the cross-field hook.
        if let Err(errs) = data.after_validation() {
            return Err(FrameworkError::Validation(errs));
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
