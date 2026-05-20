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
        <Self as suprnova::eloquent::Model>::all().await
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
