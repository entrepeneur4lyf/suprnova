//! Posts controller.
//!
//! Dogfoods the real SeaORM-backed `Post` model added by codex review
//! finding #17. Three endpoints:
//!
//! - `POST /api/posts` — authenticated create. Body is the `CreatePost`
//!   DTO (title/body/is_public). `author_id` comes from the session via
//!   `Auth::user_as::<User>()`, never from the request body.
//! - `GET  /api/posts` — list every public post (gated at the query
//!   layer via `Post::all_public`); does not require auth.
//! - `GET  /api/posts/{id}` — fetch a single post; the `PostPolicy`
//!   `view` rule (post.is_public) is enforced via `Gate::authorize`.
//!
//! `DELETE /api/posts/{id}` continues to live in
//! `controllers::admin::delete_post` because it pairs with the
//! `Gate::authorize("delete-post", ...)` example wired in
//! `app/src/policies/post_policy.rs`.

use serde::Deserialize;
use suprnova::{Auth, FrameworkError, Gate, HttpResponse, Model, Request, Response, attrs};

use crate::models::posts::Post;
use crate::models::users::User;

/// Body for `POST /api/posts`. `author_id` is intentionally omitted —
/// the server derives it from the session-authenticated user to keep
/// the trust boundary clean (a client must not be able to author a
/// post as someone else).
#[derive(Deserialize)]
pub struct CreatePost {
    pub title: String,
    pub body: String,
    /// Defaults to `false` when omitted, mirroring the column default
    /// in the migration. Public posts are visible via `GET /api/posts`.
    #[serde(default)]
    pub is_public: bool,
}

/// `POST /api/posts` — create a post owned by the current user.
pub async fn store(req: Request) -> Response {
    store_inner(req).await.map_err(HttpResponse::from)
}

async fn store_inner(req: Request) -> Result<HttpResponse, FrameworkError> {
    let current_user = Auth::user_as::<User>()
        .await?
        .ok_or(FrameworkError::Unauthorized)?;

    let payload: CreatePost = req.json().await?;

    if payload.title.trim().is_empty() {
        return Err(FrameworkError::bad_request("title must not be empty"));
    }
    if payload.body.trim().is_empty() {
        return Err(FrameworkError::bad_request("body must not be empty"));
    }

    let post = Post::create(attrs! {
        author_id: current_user.id,
        title: payload.title,
        body: payload.body,
        is_public: payload.is_public,
    })
    .await?;

    Ok(HttpResponse::json(suprnova::serde_json::json!({
        "id": post.id,
        "author_id": post.author_id,
        "title": post.title,
        "body": post.body,
        "is_public": post.is_public,
    }))
    .status(201))
}

/// `GET /api/posts` — list every public post.
pub async fn index(_req: Request) -> Response {
    index_inner().await.map_err(HttpResponse::from)
}

async fn index_inner() -> Result<HttpResponse, FrameworkError> {
    let posts = Post::all_public().await?;
    let serialized: Vec<suprnova::serde_json::Value> = posts
        .into_iter()
        .map(|p| {
            suprnova::serde_json::json!({
                "id": p.id,
                "author_id": p.author_id,
                "title": p.title,
                "body": p.body,
                "is_public": p.is_public,
            })
        })
        .collect();
    Ok(HttpResponse::json(
        suprnova::serde_json::json!({ "posts": serialized }),
    ))
}

/// `GET /api/posts/{id}` — fetch a single post, gated by the
/// `view-post` policy (`post.is_public`).
///
/// The Gate runs `view-post` registered automatically by the
/// `#[policy(User, Post)]` impl on `PostPolicy`. Anonymous viewers
/// (no session) are rejected by `Gate::authorize` because the gate
/// signature requires a `&User`.
pub async fn show(req: Request) -> Response {
    show_inner(req).await.map_err(HttpResponse::from)
}

async fn show_inner(req: Request) -> Result<HttpResponse, FrameworkError> {
    let raw_id = req.param("id")?;
    let post_id: i64 = raw_id
        .parse()
        .map_err(|_| FrameworkError::param_parse("id", "i64"))?;

    let current_user = Auth::user_as::<User>()
        .await?
        .ok_or(FrameworkError::Unauthorized)?;

    let post = Post::find_by_id(post_id)
        .await?
        .ok_or_else(|| FrameworkError::not_found("post"))?;

    Gate::authorize("view-post", &current_user, &post)?;

    Ok(HttpResponse::json(suprnova::serde_json::json!({
        "id": post.id,
        "author_id": post.author_id,
        "title": post.title,
        "body": post.body,
        "is_public": post.is_public,
    })))
}
