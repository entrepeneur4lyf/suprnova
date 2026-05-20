//! Post model — migrated to `#[suprnova::model]` in Phase 10A T11.
//!
//! Dogfoods the framework's Eloquent surface against a real DB-backed
//! entity used by `/api/posts` endpoints and the `PostPolicy`
//! authorization example. Replaces the hand-written entity + builder
//! pair the original Phase 3 dogfood shipped.

use chrono::{DateTime, Utc};
use suprnova::model;

#[model(
    table = "posts",
    fillable = ["title", "body", "is_public", "author_id"],
    timestamps,
)]
pub struct Post {
    pub id: i64,
    pub author_id: i64,
    pub title: String,
    pub body: String,
    pub is_public: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Re-export the SeaORM types the macro emits inside the per-model
// inner module — see `users.rs` for rationale.
pub use post::{ActiveModel, Column, Entity};

impl Post {
    /// Look up a post by primary key. Compatibility shim for
    /// pre-T11 callers (controllers, e2e tests). New code can use
    /// `Post::find(id)` directly.
    pub async fn find_by_id(id: i64) -> Result<Option<Self>, suprnova::FrameworkError> {
        <Self as suprnova::eloquent::Model>::find(id).await
    }

    /// Every post, ordered by id ascending. Compatibility shim.
    pub async fn all() -> Result<Vec<Self>, suprnova::FrameworkError> {
        <Self as suprnova::eloquent::Model>::query()
            .order_by_asc("id")
            .get()
            .await
    }

    /// Every public post, ordered by id ascending. Mirrors the
    /// `view-post` policy rule (`post.is_public`) at the query level
    /// so the unauthenticated listing can stream the visible subset
    /// without re-running the gate on each row.
    pub async fn all_public() -> Result<Vec<Self>, suprnova::FrameworkError> {
        <Self as suprnova::eloquent::Model>::query()
            .filter("is_public", true)
            .order_by_asc("id")
            .get()
            .await
    }

    /// Every post authored by `author_id`. Useful for the
    /// `/api/users/{id}/posts` style endpoint a real app would add.
    pub async fn for_author(author_id: i64) -> Result<Vec<Self>, suprnova::FrameworkError> {
        <Self as suprnova::eloquent::Model>::query()
            .filter("author_id", author_id)
            .order_by_asc("id")
            .get()
            .await
    }
}
