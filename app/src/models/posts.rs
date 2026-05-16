//! Post model
//!
//! Real SeaORM-backed model used by the dogfood `/api/posts` endpoints
//! and the `PostPolicy` authorization example. Replaces the hardcoded
//! stub that codex review finding #17 flagged as weakening dogfood
//! confidence.
//!
//! The base entity is hand-mirrored in `src/models/entities/posts.rs`
//! and is NEVER overwritten by `suprnova db:sync` — custom finders and
//! builders live here. The columns match
//! `app/src/migrations/m20251208_240000_create_posts_table.rs`.

// Re-export the entity so callers can import `Column`, `Entity`,
// `ActiveModel`, and `Model` as `crate::models::posts::*` exactly the
// way users.rs and todos.rs work.
pub use super::entities::posts::*;

use sea_orm::{entity::prelude::*, Set};
use suprnova::database::{ModelMut, QueryBuilder};

/// Type alias matching the existing dogfood naming
/// (`Post` rather than `Model`). PostPolicy and the admin controller
/// both depend on this name.
pub type Post = Model;

// ── Entity configuration ───────────────────────────────────────────

impl ActiveModelBehavior for ActiveModel {}

impl suprnova::database::Model for Entity {}
impl suprnova::database::ModelMut for Entity {}

// ── Eloquent-style API ─────────────────────────────────────────────

impl Model {
    /// Start a fluent query builder bound to this entity.
    pub fn query() -> QueryBuilder<Entity> {
        QueryBuilder::new()
    }

    /// Begin a new-row builder.
    pub fn create() -> PostBuilder {
        PostBuilder::default()
    }

    /// Look up a post by primary key. Returns `Ok(None)` if no row
    /// matches — the controller layer maps that to a 404.
    pub async fn find_by_id(id: i32) -> Result<Option<Self>, suprnova::FrameworkError> {
        Self::query().filter(Column::Id.eq(id)).first().await
    }

    /// Return every post, ordered by id ascending. Used by the
    /// admin/listing controllers (PostPolicy still gates per-row
    /// access at the application layer).
    pub async fn all() -> Result<Vec<Self>, suprnova::FrameworkError> {
        Self::query().order_by_asc(Column::Id).all().await
    }

    /// Return every public post, ordered by id ascending. Mirrors the
    /// `view-post` policy rule (`post.is_public`) at the query level
    /// so an unauthenticated `GET /api/posts/public` endpoint can
    /// stream the visible subset without re-running the gate on each
    /// row.
    pub async fn all_public() -> Result<Vec<Self>, suprnova::FrameworkError> {
        Self::query()
            .filter(Column::IsPublic.eq(true))
            .order_by_asc(Column::Id)
            .all()
            .await
    }

    /// Return every post authored by `author_id`. Useful for the
    /// `/api/users/{id}/posts` style endpoint a real app would add;
    /// the dogfood doesn't ship one but the helper keeps the model's
    /// surface complete.
    pub async fn for_author(author_id: i32) -> Result<Vec<Self>, suprnova::FrameworkError> {
        Self::query()
            .filter(Column::AuthorId.eq(author_id))
            .order_by_asc(Column::Id)
            .all()
            .await
    }

    /// Delete this row, returning the number of rows affected
    /// (always 1 on success). Matches the existing dogfood usage in
    /// `admin::delete_post` which drops the return value.
    pub async fn delete(self) -> Result<u64, suprnova::FrameworkError> {
        Entity::delete_by_pk(self.id).await
    }
}

// ── Builder ────────────────────────────────────────────────────────

/// Builder for creating new Post records.
///
/// `created_at` / `updated_at` are populated by SQL defaults
/// (`Expr::current_timestamp`) when not set — see the migration. The
/// `author_id`, `title`, and `body` fields are mandatory and the
/// builder panics at `insert()` if any is missing.
#[derive(Default)]
pub struct PostBuilder {
    author_id: Option<i32>,
    title: Option<String>,
    body: Option<String>,
    is_public: Option<bool>,
}

impl PostBuilder {
    /// Set the author user id. Required.
    pub fn set_author_id(mut self, value: i32) -> Self {
        self.author_id = Some(value);
        self
    }

    /// Set the post title. Required.
    pub fn set_title(mut self, value: impl Into<String>) -> Self {
        self.title = Some(value.into());
        self
    }

    /// Set the post body. Required.
    pub fn set_body(mut self, value: impl Into<String>) -> Self {
        self.body = Some(value.into());
        self
    }

    /// Set the `is_public` flag. Defaults to `false` per the migration.
    pub fn set_is_public(mut self, value: bool) -> Self {
        self.is_public = Some(value);
        self
    }

    /// Insert the row and return the persisted [`Post`]. Errors flow
    /// through `FrameworkError::database` so the controller can
    /// surface a 5xx without leaking the SeaORM message (codex
    /// review finding #2 already sanitises 5xx payloads).
    pub async fn insert(self) -> Result<Model, suprnova::FrameworkError> {
        let active = self.build()?;
        Entity::insert_one(active).await
    }

    fn build(self) -> Result<ActiveModel, suprnova::FrameworkError> {
        let author_id = self.author_id.ok_or_else(|| {
            suprnova::FrameworkError::bad_request("Post::create: author_id is required")
        })?;
        let title = self.title.ok_or_else(|| {
            suprnova::FrameworkError::bad_request("Post::create: title is required")
        })?;
        let body = self.body.ok_or_else(|| {
            suprnova::FrameworkError::bad_request("Post::create: body is required")
        })?;
        Ok(ActiveModel {
            id: sea_orm::ActiveValue::NotSet,
            author_id: Set(author_id),
            title: Set(title),
            body: Set(body),
            is_public: Set(self.is_public.unwrap_or(false)),
            // SQL defaults fill these in.
            created_at: sea_orm::ActiveValue::NotSet,
            updated_at: sea_orm::ActiveValue::NotSet,
        })
    }
}
