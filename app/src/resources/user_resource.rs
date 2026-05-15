//! JSON:API resource representation for User.
//!
//! Exercises the Phase 3 `#[derive(Data)] #[json_resource("users")]` pipeline.
//!
//! **Schema note:** The dogfood `users` table has only `id`, `created_at`,
//! and `updated_at`. There is no `email` column.  `email` is synthesised
//! from the user's id so the JSON:API attributes object has a non-trivial
//! field without requiring a migration. `password` is an input-only field —
//! it is excluded from serialised output by `#[data(input_only)]`.

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
    /// Primary key.  JSON:API emits this as the `id` member (string).
    pub id: i32,

    /// Synthesised e-mail address. The dogfood entity has no email column;
    /// this is derived at conversion time so the attributes object is
    /// non-trivial.
    pub email: String,

    /// ISO-8601 timestamp sourced from the entity's `created_at` string.
    pub created_at: String,

    /// Raw password — accepted on input, never serialised to output.
    #[data(input_only)]
    pub password: String,
}

impl From<User> for UserResource {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            email: format!("user-{}@example.suprnova.app", u.id),
            created_at: u.created_at.clone(),
            password: String::new(),
        }
    }
}
