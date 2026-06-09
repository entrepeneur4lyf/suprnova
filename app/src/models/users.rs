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
use suprnova::eloquent::attrs::Attrs;
use suprnova::eloquent::events::EventResult;
use suprnova::eloquent::observers::Observer;
use suprnova::{Authenticatable, CanResetPassword, FrameworkError, MustVerifyEmail, model};

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
    // Nullable verification timestamp powering the email-verification flow.
    // The model macro auto-injects `AsOptionalDateTime` for
    // `Option<DateTime<Utc>>` fields, so no explicit cast entry is needed.
    pub email_verified_at: Option<DateTime<Utc>>,
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
    fn get_auth_identifier(&self) -> String {
        self.id.to_string()
    }

    fn auth_identifier_name(&self) -> &'static str {
        "id"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_arc_any(self: std::sync::Arc<Self>) -> std::sync::Arc<dyn Any + Send + Sync> {
        self
    }
}

// The email-verification flow reads the address + verification timestamp
// through this trait and writes the timestamp back on consume. Implementing
// it (alongside `CanResetPassword` below) is what lets the
// `EloquentUserProvider<User>` registered in `bootstrap::register()` drive
// `EmailVerification::resend` / `verify` against this model.
impl MustVerifyEmail for User {
    fn email(&self) -> &str {
        &self.email
    }

    fn email_verified_at(&self) -> Option<DateTime<Utc>> {
        self.email_verified_at
    }

    fn set_email_verified_at(&mut self, v: Option<DateTime<Utc>>) {
        self.email_verified_at = v;
    }

    fn name(&self) -> Option<&str> {
        Some(&self.name)
    }
}

// The password-reset flow addresses its mail through `email_for_reset()` and
// persists the rotated (already-hashed) password through `set_password_hash()`.
impl CanResetPassword for User {
    fn email_for_reset(&self) -> &str {
        &self.email
    }

    fn set_password_hash(&mut self, hash: &str) {
        self.password = hash.to_string();
    }
}

// ---- Phase 10C T14 — local scope dogfood ---------------------------------
//
// `active` is the canonical Eloquent local-scope demo. The macro emits:
//
//   1. A static helper: `User::active().get().await?` starts a builder
//      pre-filtered to active rows.
//   2. A `Builder<User>` extension trait method: `User::query().filter(...).active()`
//      composes onto an existing chain.
//
// Pattern follows `framework/tests/eloquent_scopes_local.rs` — the
// dogfood exists to prove the macro path works on a real app model, not
// just the framework's own test fixtures.

#[suprnova::scopes(User)]
impl User {
    /// Scope: only active (non-disabled) users.
    ///
    /// The `active` column ships in the Phase 10A T11 user-columns
    /// migration with `DEFAULT TRUE`, so every freshly-created user is
    /// caught by this scope until something explicitly flips the flag.
    pub fn active(query: suprnova::Builder<Self>) -> suprnova::Builder<Self> {
        query.filter("active", true)
    }
}

// ---- Phase 10C T14 — Observer<User> dogfood ------------------------------
//
// `UserObserver` exercises T2's observer surface end-to-end against a
// real app model. Two methods are overridden:
//
// - `creating` normalises the email column to lower-case before insert.
//   Demonstrates the cancellable family: it returns `EventResult` so an
//   observer could veto the create by returning `EventResult::cancel`.
// - `created` logs the new row's id via `tracing::info!`. Demonstrates
//   the non-cancellable family — fires AFTER the insert lands.
//
// The `#[suprnova::observer(User)]` attribute walks this impl block at
// parse time and emits exactly the listener adapters for the two
// overridden methods. The other 14 `Observer<User>` defaults are
// untouched; no spurious listeners get registered for them.
//
// Wired into `app::bootstrap::register()` via
// `suprnova::eloquent::observers::bootstrap_observers().await`.

pub struct UserObserver;

#[suprnova::observer(User)]
#[async_trait::async_trait]
impl Observer<User> for UserObserver {
    /// Lower-case the `email` column before the row is written. Real
    /// apps reach for this to keep `WHERE email = ?` lookups consistent
    /// when users sign up from a phone keyboard that auto-caps the
    /// first letter.
    async fn creating(&self, attrs: &mut Attrs) -> EventResult {
        if let Some(email) = attrs.get("email").and_then(|v| v.as_str()) {
            let lowered = email.to_lowercase();
            if lowered != email {
                attrs.insert("email", suprnova::serde_json::Value::String(lowered));
            }
        }
        EventResult::ok()
    }

    /// Trace every successful user creation. Hooks for analytics,
    /// welcome-email queueing, etc. would go here in a real app.
    async fn created(&self, user: &User) -> Result<(), FrameworkError> {
        tracing::info!(
            target: "app::user_observer",
            user_id = user.id,
            email = %user.email,
            "user created",
        );
        Ok(())
    }
}
