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

/// Row in the framework-owned `features` table.
///
/// `(name, scope_key)` is a UNIQUE composite key; `scope_key = ""` means a
/// global flag, other values carry the scope inline as `kind:identifier`.
#[suprnova::model(table = "features", timestamps)]
pub struct Feature {
    /// Primary key.
    pub id: i64,
    /// Flag identifier (e.g. `"checkout.v2"`).
    pub name: String,
    /// Scope discriminator; empty string for the global default, otherwise `kind:identifier`.
    pub scope_key: String,
    /// Whether the flag resolves to enabled for this `(name, scope_key)` pair.
    pub enabled: bool,
    /// Operator-facing description shown in the admin UI.
    pub description: Option<String>,
    /// Opaque identifier of the user who last toggled the flag, or `NULL` for system changes.
    pub updated_by: Option<String>,
    /// Timestamp at which the row was inserted.
    pub created_at: DateTime<Utc>,
    /// Timestamp at which the row was last mutated.
    pub updated_at: DateTime<Utc>,
}

// Re-export the SeaORM types the macro emits inside the per-struct
// inner module so the rest of the `features` module (admin.rs,
// evaluators/database.rs) keeps reaching `entity::Entity`,
// `entity::Column`, `entity::ActiveModel` exactly the way it did
// before T11. The user-facing `Feature` struct is also re-exported as
// `Model` for backwards-compatibility with any code that referenced
// `entity::Model` (the old hand-rolled name).
pub use feature::Model;
pub use feature::{ActiveModel, Column, Entity};
