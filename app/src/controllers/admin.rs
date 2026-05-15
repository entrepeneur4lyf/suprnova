//! Phase 3 dogfood: JSON:API resources + Gate-authorized deletion.
//!
//! Three endpoints:
//!
//! * `GET /api/users/{id}` — JSON:API single-resource envelope via
//!   `Resource::single`. Supports `?fields[users]=...` sparse fieldsets
//!   scoped by `IncludeMiddleware`.
//!
//! * `GET /api/v3/users` — JSON:API collection envelope via
//!   `Resource::collection`.  Same sparse-fieldset support.
//!
//! * `DELETE /api/posts/{id}` — demonstrates `Gate::authorize` with the
//!   `PostPolicy`. The "current user" is loaded from a stub (user id=1)
//!   because `Auth::user_as::<User>()` doesn't work cleanly against the
//!   torii-backed session layer for this dogfood entity (see P3T8 known
//!   limitation). In a real app with a proper session-user mapping this
//!   would be `Auth::user_as::<User>().await?.ok_or(FrameworkError::Unauthorized)?`.

use crate::models::{posts::Post, users::User};
use crate::resources::user_resource::UserResource;
use suprnova::{FrameworkError, Gate, HttpResponse, Request, Resource, Response};

// ---------------------------------------------------------------------------
// GET /api/users/{id}
// ---------------------------------------------------------------------------

/// Return a single user as a JSON:API resource object.
///
/// Sparse fieldsets are applied automatically by `IncludeMiddleware`;
/// consumers pass `?fields[users]=email` to receive only the listed
/// attributes.
pub async fn show_user(req: Request) -> Response {
    show_user_inner(req).await.map_err(HttpResponse::from)
}

async fn show_user_inner(req: Request) -> Result<HttpResponse, FrameworkError> {
    let raw_id = req.param("id")?;
    let user_id: i32 = raw_id
        .parse()
        .map_err(|_| FrameworkError::param_parse("id", "i32"))?;

    let user = User::find_by_id(user_id)
        .await?
        .ok_or_else(|| FrameworkError::not_found("user"))?;

    Resource::single(UserResource::from(user)).render().await
}

// ---------------------------------------------------------------------------
// GET /api/v3/users
// ---------------------------------------------------------------------------

/// Return all users as a JSON:API collection.
///
/// Sparse fieldsets work the same as on the single-resource endpoint.
pub async fn list_users(_req: Request) -> Response {
    list_users_inner().await.map_err(HttpResponse::from)
}

async fn list_users_inner() -> Result<HttpResponse, FrameworkError> {
    let users = User::find_all().await?;
    let resources: Vec<UserResource> = users.into_iter().map(UserResource::from).collect();
    Resource::collection(resources).render().await
}

// ---------------------------------------------------------------------------
// DELETE /api/posts/{id}
// ---------------------------------------------------------------------------

/// Delete a post after authorizing via `Gate::authorize("delete-post", ...)`.
///
/// The gate is registered automatically at boot via the
/// `#[policy(User, Post)]` impl on `PostPolicy` (inventory-based
/// registration). If the current user doesn't own the post the gate
/// returns `Err(FrameworkError::Unauthorized)` which is mapped to 403.
///
/// **Dogfood stub:** the "current user" is always user id=1 because the
/// torii session layer stores a string UserId that can't be cleanly mapped
/// back to the SeaORM `User` entity in this demo (P3T8 known limitation).
pub async fn delete_post(req: Request) -> Response {
    delete_post_inner(req).await.map_err(HttpResponse::from)
}

async fn delete_post_inner(req: Request) -> Result<HttpResponse, FrameworkError> {
    let raw_id = req.param("id")?;
    let post_id: i32 = raw_id
        .parse()
        .map_err(|_| FrameworkError::param_parse("id", "i32"))?;

    // Dogfood: load user id=1 as the "current user".
    // A real app would call `Auth::user_as::<User>().await?`.
    let current_user = User::find_by_id(1)
        .await?
        .ok_or(FrameworkError::Unauthorized)?;

    let post = Post::find_by_id(post_id)
        .await?
        .ok_or_else(|| FrameworkError::not_found("post"))?;

    Gate::authorize("delete-post", &current_user, &post)?;
    post.delete().await?;

    Ok(HttpResponse::json(suprnova::serde_json::json!({ "deleted": true })))
}
