//! Taggable — polymorphic pivot for Tag ↔ (Post | Video).
//!
//! Unlike the `role_user` pivot for the User/Role m2m, this one carries
//! a `<name>_type` column on top of the FK pair. The pivot itself is
//! still a `#[suprnova::model]` — the polymorphic m2m machinery treats
//! it like any other pivot, just with one extra column the loader
//! includes in its `<rel>_type = '<target>'` filter.
//!
//! No timestamps on this pivot — the schema deliberately omits them so
//! the dogfood proves both shapes (with and without timestamps on
//! pivots) work. `role_user` is the "with timestamps" branch.

use suprnova::model;

#[model(
    table = "taggables",
    fillable = ["tag_id", "taggable_id", "taggable_type"],
)]
pub struct Taggable {
    pub id: i64,
    pub tag_id: i64,
    pub taggable_id: i64,
    pub taggable_type: String,
}

pub use taggable::{ActiveModel, Column, Entity};
