//! SeaORM entity for the framework-owned `features` table.
//!
//! Schema lives in [`crate::features::migrations::CreateFeaturesTable`].
//! The shape mirrors the migration column-for-column:
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

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "features")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub name: String,
    pub scope_key: String,
    pub enabled: bool,
    #[sea_orm(column_type = "Text", nullable)]
    pub description: Option<String>,
    pub updated_by: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
