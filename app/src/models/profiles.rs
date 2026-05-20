//! Profile model — Phase 10B P5 dogfood for `HasOne`.
//!
//! Sits on the "child" side of a one-to-one relation with `User`:
//! each Profile row carries a `user_id` FK back to its owning User,
//! and the `user_id` column is UNIQUE in the schema so a User has
//! at most one Profile (by definition of HasOne).
//!
//! Paired with `User.profile: HasOne<Profile>` on the parent side —
//! see `crate::models::users`. The default FK convention
//! (`default_has_fk("User") == "user_id"`) matches this schema, so
//! the parent's relation declaration carries no `fk = "..."` override.
//!
//! The inverse `BelongsTo<User>` is declared here for completeness
//! and to keep the dogfood symmetrical with the other relation
//! models (`Comment.commentable`, `Role.users`, etc.).

use chrono::{DateTime, Utc};
use suprnova::model;

#[model(
    table = "profiles",
    fillable = ["user_id", "bio"],
    timestamps,
    relations = {
        user: BelongsTo<crate::models::users::User>,
    },
)]
pub struct Profile {
    pub id: i64,
    pub user_id: i64,
    pub bio: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Re-export the SeaORM types the macro emits inside the inner module
// so call sites that want the raw SeaORM `Entity` etc. don't need a
// separate `use profile::*`. Same pattern Phase 10A T11 established
// on `User` / `Post` and Phase 10B T10 extended to `Role` et al.
pub use profile::{ActiveModel, Column, Entity};
