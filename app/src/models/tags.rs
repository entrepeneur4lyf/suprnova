//! Tag — Phase 10B T10 dogfood for `MorphedByMany`.
//!
//! Sits on the "many side that points back to many families" of a
//! polymorphic many-to-many. The shared `taggables` pivot table
//! carries `(tag_id, taggable_id, taggable_type)`; Tag declares two
//! `MorphedByMany` relations — one each for Posts and Videos — and
//! the loader filters by `taggable_type` to keep the two families
//! separate.
//!
//! `target_morph_type = "post"` / `"video"` on each relation is what
//! tells the loader which type-string each branch should match against
//! when reading the pivot. The relation name (`"taggable"`) drives the
//! `<name>_id` / `<name>_type` column lookup on the pivot side.

use chrono::{DateTime, Utc};
use suprnova::model;

#[model(
    table = "tags",
    fillable = ["name"],
    timestamps,
    relations = {
        posts: MorphedByMany<crate::models::posts::Post, crate::models::taggables::Taggable> {
            name = "taggable",
            target_morph_type = "post",
        },
        videos: MorphedByMany<crate::models::videos::Video, crate::models::taggables::Taggable> {
            name = "taggable",
            target_morph_type = "video",
        },
    },
)]
pub struct Tag {
    pub id: i64,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub use tag::{ActiveModel, Column, Entity};
