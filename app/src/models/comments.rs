//! Comment — Phase 10B T10 dogfood for `MorphTo`.
//!
//! Sits on the "child" side of the polymorphic relation: each Comment
//! row carries a (`commentable_id`, `commentable_type`) pair that
//! points at either a Post or a Video. The `MorphTo` declaration's
//! `targets = [Post, Video]` is what causes the macro to emit the
//! per-family enum `CommentableMorph` with variants for each, plus an
//! `Unknown(type_string, id)` fallback for legacy rows whose
//! `commentable_type` value doesn't match any registered target.
//!
//! `Comment::commentable()` returns a `CommentableMorphFetch` whose
//! `.get().await?` resolves the parent and yields the per-family enum
//! — callers `match` on the enum to recover the concrete target.

use chrono::{DateTime, Utc};
use suprnova::model;

#[model(
    table = "comments",
    fillable = ["commentable_id", "commentable_type", "body"],
    timestamps,
    relations = {
        commentable: MorphTo {
            name = "commentable",
            targets = [
                crate::models::posts::Post,
                crate::models::videos::Video,
            ],
        },
    },
)]
pub struct Comment {
    pub id: i64,
    pub commentable_id: i64,
    pub commentable_type: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub use comment::{ActiveModel, Column, Entity};
