//! Video — Phase 10B T10 dogfood for the second branch of the
//! polymorphic Comment family.
//!
//! Carrying `morph_type = "video"` is what registers the model in
//! `framework::eloquent::relations::morph_types()`. The string is what
//! the polymorphic loaders stamp into the child's `<name>_type` column
//! (e.g. `comments.commentable_type = "video"`).
//!
//! The `comments` relation mirrors Post's: same morph name
//! (`"commentable"`), different parent. The morph machinery filters
//! by `commentable_type = "<parent>.morph_type"` so the two branches
//! stay independent — `video.comments()` never returns a Post's
//! comments and vice-versa.
//!
//! The `tags` relation lands a `MorphToMany` on Video too — Tags can
//! be attached to Posts or Videos through the shared `taggables`
//! pivot, exercising the polymorphic m2m surface with a second target
//! family.

use chrono::{DateTime, Utc};
use suprnova::model;

#[model(
    table = "videos",
    fillable = ["url"],
    morph_type = "video",
    timestamps,
    relations = {
        comments: MorphMany<crate::models::comments::Comment> {
            name = "commentable",
        },
        tags: MorphToMany<crate::models::tags::Tag, crate::models::taggables::Taggable> {
            name = "taggable",
        },
    },
)]
pub struct Video {
    pub id: i64,
    pub url: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub use video::{ActiveModel, Column, Entity};
