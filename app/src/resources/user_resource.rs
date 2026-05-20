//! JSON:API resource representation for User.
//!
//! Exercises the Phase 3 `#[derive(Data)] #[json_resource("users")]` pipeline.
//!
//! Phase 10A T11 — `User` now carries real identity columns (name,
//! email, …) since the dogfood migrated to `#[suprnova::model]`. The
//! resource projects only the fields safe for serialisation; the
//! `password` column stays input-only.

use crate::models::users::User;
use suprnova::Data;
use validator::Validate;

/// JSON:API `users` resource.
///
/// The `#[json_resource("users")]` attribute causes `#[derive(Data)]` to emit
/// an `IntoJsonResource` impl that wraps instances in the standard JSON:API
/// resource-object envelope:
///
/// ```json
/// {
///   "data": {
///     "type": "users",
///     "id": "42",
///     "attributes": { "email": "...", "created_at": "..." }
///   }
/// }
/// ```
///
/// Sparse fieldsets (`?fields[users]=email`) and compound-document includes
/// are handled automatically by `IncludeMiddleware` + `Resource::single`.
#[derive(Debug, Clone, Data, Validate)]
#[json_resource("users")]
pub struct UserResource {
    /// Primary key. JSON:API emits this as the `id` member (string).
    pub id: i64,

    /// E-mail address. Populated from the `User::email` column after
    /// Phase 10A T11's migration; the previous shim that synthesised
    /// the value from the user id is no longer needed.
    pub email: String,

    /// ISO-8601 timestamp string. Stringified at conversion time from
    /// the underlying `DateTime<Utc>` so the JSON:API envelope keeps
    /// its existing string-typed `created_at` shape.
    pub created_at: String,

    /// Raw password — accepted on input, never serialised to output.
    #[data(input_only)]
    pub password: String,
}

impl From<User> for UserResource {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            email: u.email,
            created_at: u.created_at.to_rfc3339(),
            password: String::new(),
        }
    }
}
