//! Role model — Phase 10B T10 dogfood for `BelongsToMany`.
//!
//! Pairs with [`RoleUser`](super::role_user::RoleUser) as the pivot.
//! `roles` is the "right side" of a many-to-many — every User can hold
//! many Roles via the `role_user` pivot table. The matching declaration
//! on `User` carries `with_pivot = ["assigned_at"]` so the pivot's
//! own non-key column flows back as `roles[0].pivot::<RoleUser>()`.

use chrono::{DateTime, Utc};
use suprnova::model;

#[model(
    table = "roles",
    fillable = ["name"],
    timestamps,
    relations = {
        users: BelongsToMany<crate::models::users::User, crate::models::role_user::RoleUser> {
            with_pivot = ["assigned_at"],
            with_timestamps,
        },
    },
)]
pub struct Role {
    pub id: i64,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Re-export the SeaORM types the macro emits inside the inner module
// so call sites that want the raw SeaORM `Entity` etc. don't need a
// separate `use role::*`. Same pattern Phase 10A T11 established on
// `User` and `Post`.
pub use role::{ActiveModel, Column, Entity};
