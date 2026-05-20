//! Suprnova model for the framework-owned `features` table.
//!
//! Phase 10A T11 migrated this entity to `#[suprnova::model]`, the same
//! macro consumers use. The shape mirrors the migration column-for-column:
//!
//! * `name` + `scope_key` form a UNIQUE composite key. `scope_key = ""`
//!   represents a global flag; other values like `"user:42"` or
//!   `"team:staff"` carry the scope inline as a `kind:identifier`
//!   string so the read path stays a single string lookup.
//! * `description` is operator-facing context for the admin UI.
//! * `updated_by` is a nullable audit pointer to the user who last
//!   toggled the flag (NULL for system-initiated changes). String-typed
//!   to carry torii's opaque (UUID / ULID) user ids — numeric-keyed apps
//!   round-trip via `Option<String>` without loss.
//!
//! Schema lives in [`crate::features::migrations::CreateFeaturesTable`].
//! Other code in the `features` module reaches the SeaORM types via the
//! re-exports below (`Entity`, `Column`, `ActiveModel`) so existing
//! call sites in `admin.rs` / `evaluators/database.rs` keep working
//! unchanged.

use chrono::{DateTime, Utc};

#[suprnova::model(
    table = "features",
    timestamps,
)]
pub struct Feature {
    pub id: i64,
    pub name: String,
    pub scope_key: String,
    pub enabled: bool,
    pub description: Option<String>,
    pub updated_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Re-export the SeaORM types the macro emits inside the per-struct
// inner module so the rest of the `features` module (admin.rs,
// evaluators/database.rs) keeps reaching `entity::Entity`,
// `entity::Column`, `entity::ActiveModel` exactly the way it did
// before T11. The user-facing `Feature` struct is also re-exported as
// `Model` for backwards-compatibility with any code that referenced
// `entity::Model` (the old hand-rolled name).
pub use feature::{ActiveModel, Column, Entity};
pub use feature::Model;
