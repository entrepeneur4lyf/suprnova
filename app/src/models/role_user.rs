//! RoleUser — pivot model for the `users <-> roles` many-to-many.
//!
//! Pivot models are first-class `#[suprnova::model]` types — same
//! macro, same Eloquent surface. The only thing that makes this one a
//! "pivot" is that `User::roles` and `Role::users` reference it in
//! their `BelongsToMany<Other, RoleUser> { ... }` declaration.
//!
//! The extra `assigned_at` column is the pivot context: it is NOT a
//! key column, but `with_pivot = ["assigned_at"]` on the parent
//! declaration tells the loader to SELECT it alongside the keys so
//! callers can read it via `related.pivot::<RoleUser>().assigned_at`.

use chrono::{DateTime, Utc};
use suprnova::model;

#[model(
    table = "role_user",
    fillable = ["user_id", "role_id", "assigned_at"],
    timestamps,
)]
pub struct RoleUser {
    pub id: i64,
    pub user_id: i64,
    pub role_id: i64,
    pub assigned_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub use role_user::{ActiveModel, Column, Entity};
