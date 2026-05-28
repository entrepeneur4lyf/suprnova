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
pub trait FormRequest: Sized + DeserializeOwned + Validate + Send + Sync {
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

    /// Async cross-field validation hook. This is where database-backed
    /// and other `.await`-ing rules — most notably the built-in
    /// [`Unique`] rule — participate in automatic request validation.
    ///
    /// The synchronous [`validate!`] macro cannot weave in `.await`
    /// points, and [`after_validation`] is synchronous, so without this
    /// hook an async rule like `Unique` could only run if every app
    /// hand-wrote the same plumbing in its handler. `extract` calls this
    /// method as the final validation stage, so overriding it is all an
    /// app needs:
    ///
    /// ```rust,ignore
    /// #[async_trait]
    /// impl FormRequest for CreateUserRequest {
    ///     async fn after_validation_async(&self) -> Result<(), ValidationErrors> {
    ///         let mut errs = ValidationErrors::new();
    ///         Unique::new("users", "email")
    ///             .check_async(&self.email, &mut errs, "email")
    ///             .await;
    ///         errs.into_result()
    ///     }
    /// }
    /// ```
    ///
    /// # Ordering and bail behavior
    ///
    /// `extract` runs the stages in order — the derived `validate()`, the
    /// synchronous [`after_validation`], then this async hook — and
    /// **bails at the first failing stage**. The async hook therefore
    /// only runs once the synchronous rules pass, so a malformed value
    /// (e.g. a syntactically invalid email) never reaches the database
    /// `Unique` query. In Precognition mode the hook's errors are
    /// filtered by `Precognition-Validate-Only` exactly like the other
    /// stages.
    ///
    /// The default implementation returns `Ok(())`.
    ///
    /// [`Unique`]: crate::validation::rule::async_rules::Unique
    /// [`validate!`]: crate::validate
    /// [`after_validation`]: Self::after_validation
    async fn after_validation_async(&self) -> Result<(), ValidationErrors> {
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
            // Walk the validation stages in order, bailing at the first
            // that fails: the derived `validate()`, then the synchronous
            // cross-field hook, then the async cross-field hook (where
            // `Unique` and other DB-backed rules live). The async stage
            // runs only once the cheaper synchronous stages pass, so a
            // malformed value never reaches a database `Unique` query —
            // and if a *different* field is malformed, the client's
            // requested field still resolves through the same filter. An
            // empty bag means every stage passed.
            let bag = match validation_result {
                Err(errors) => ValidationErrors::from_validator(errors),
                Ok(()) => match data.after_validation() {
                    Err(errs) => errs,
                    Ok(()) => match data.after_validation_async().await {
                        Err(errs) => errs,
                        Ok(()) => ValidationErrors::new(),
                    },
                },
            };
            return Err(precognition_outcome(bag, &validate_only));
        }

        // Non-Precognition: standard flow. Same staged, bail-on-first
        // structure as the Precognition branch above.
        if let Err(errors) = validation_result {
            return Err(FrameworkError::Validation(
                ValidationErrors::from_validator(errors),
            ));
        }

        // Per-field rules passed — run the synchronous cross-field hook.
        if let Err(errs) = data.after_validation() {
            return Err(FrameworkError::Validation(errs));
        }

        // Synchronous stages passed — run the async cross-field hook
        // (DB-backed rules such as `Unique`). This is the final stage.
        if let Err(errs) = data.after_validation_async().await {
            return Err(FrameworkError::Validation(errs));
        }

        Ok(data)
    }
}

/// Collapse a (possibly empty) validation error bag into the Precognition
/// outcome: filter to the `Precognition-Validate-Only` fields (when the
/// client supplied a filter), then map an empty result to
/// [`FrameworkError::PrecognitionSuccess`] (HTTP 204) and a non-empty one
/// to [`FrameworkError::PrecognitionFailure`] (HTTP 422). An empty input
/// bag — every validation stage passed — is always success.
fn precognition_outcome(bag: ValidationErrors, validate_only: &[String]) -> FrameworkError {
    let filtered = if validate_only.is_empty() {
        bag
    } else {
        bag.retain_fields(validate_only)
    };
    if filtered.is_empty() {
        FrameworkError::PrecognitionSuccess
    } else {
        FrameworkError::PrecognitionFailure(filtered)
    }
}

/// Blanket implementation of FromRequest for all FormRequest types
#[async_trait]
impl<T: FormRequest> FromRequest for T {
    async fn from_request(req: Request) -> Result<Self, FrameworkError> {
        T::extract(req).await
    }
}
