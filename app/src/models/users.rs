//! User model — migrated to `#[suprnova::model]` in Phase 10A T11.
//!
//! The struct replaces the hand-written SeaORM entity + builder pair
//! that the old auto-generated `entities/users.rs` shipped. The macro
//! emits an inner `user` module with the SeaORM `Entity` / `Column` /
//! `ActiveModel` types alongside the user-facing `User` struct itself
//! (which carries the Eloquent surface — `create`, `find`, `query`,
//! `update`, `delete`, soft-delete lifecycle, mass-assignment, the
//! `AsBool` cast on `active`, etc.).
//!
//! `Authenticatable` is implemented on the user-facing `User` struct
//! so the rest of the auth stack (session middleware, providers,
//! `Auth::user()`) keeps working without touching the SeaORM layer.

use chrono::{DateTime, Utc};
use std::any::Any;
use suprnova::{model, Authenticatable};

#[model(
    table = "users",
    fillable = ["name", "email", "password"],
    hidden = ["password", "remember_token"],
    casts = {
        active = ::suprnova::AsBool,
    },
    soft_deletes,
    timestamps,
    // Phase 10B T10 — relations declarations drive `posts()` /
    // `roles()` accessors + the eager-load dispatcher arms.
    //
    // - `posts` is a HasMany over the `author_id` FK. The default
    //   convention would be `user_id`, but the legacy posts schema
    //   uses `author_id` (the column was named for the policy gate
    //   in Phase 3) — `fk = "author_id"` keeps the dogfood honest
    //   without backfilling the schema.
    // - `roles` is a BelongsToMany via the `RoleUser` pivot. The
    //   `with_pivot = ["assigned_at"]` directive includes the
    //   pivot's extra column in the join so `role.pivot::<RoleUser>()`
    //   surfaces it on the loaded rows.
    // - `profile` is a HasOne (Phase 10B P5) — exactly one Profile
    //   per User, FK defaults to `user_id` on the child table. The
    //   `profiles.user_id` column carries a UNIQUE constraint at the
    //   schema level so the "at most one" invariant is enforced even
    //   if direct SQL bypasses the model.
    relations = {
        posts: HasMany<crate::models::posts::Post> {
            fk = "author_id",
        },
        roles: BelongsToMany<crate::models::roles::Role, crate::models::role_user::RoleUser> {
            with_pivot = ["assigned_at"],
            with_timestamps,
        },
        profile: HasOne<crate::models::profiles::Profile>,
    },
)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub password: String,
    pub remember_token: Option<String>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

// Re-export the SeaORM types the macro emits inside the per-model
// inner module so older call sites that referenced
// `crate::models::users::{Entity, Column, ActiveModel}` keep
// resolving. New code can reach through `user::*` directly.
pub use user::{ActiveModel, Column, Entity};

impl User {
    /// Look up a user by primary key. Thin wrapper around
    /// `Model::find` kept for backwards-compatibility with the pre-T11
    /// call sites (auth provider, admin controller). New code should
    /// prefer `User::find` directly.
    pub async fn find_by_id(id: i64) -> Result<Option<Self>, suprnova::FrameworkError> {
        <Self as suprnova::eloquent::Model>::find(id).await
    }

    /// Whether this user holds admin privileges.
    ///
    /// The dogfood schema doesn't persist this flag yet — returning
    /// `false` keeps the `PostPolicy` admin-bypass branch covered by
    /// the gate tests without requiring an additional migration. A
    /// real app would migrate an `is_admin` boolean column and read
    /// it here.
    pub fn is_admin(&self) -> bool {
        false
    }

    /// Compatibility alias for the pre-T11 builder-style listing.
    pub async fn find_all() -> Result<Vec<Self>, suprnova::FrameworkError> {
        Ok(<Self as suprnova::eloquent::Model>::all().await?.into_vec())
    }
}

impl Authenticatable for User {
    fn auth_identifier(&self) -> i64 {
        self.id
    }

    fn auth_identifier_name(&self) -> &'static str {
        "id"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
