//! Framework-owned migrations for the feature-flags subsystem.
//!
//! Re-exported under [`crate::features::migrations`] so consumer apps
//! can register the schema in their own `Migrator`:
//!
//! ```rust,no_run
//! use suprnova::features::migrations::CreateFeaturesTable;
//! # struct Migrator;
//!
//! impl sea_orm_migration::MigratorTrait for Migrator {
//!     fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
//!         vec![Box::new(CreateFeaturesTable)]
//!     }
//! }
//! ```

pub mod m_create_features_table;

/// Public alias so consumers can write `CreateFeaturesTable` instead of
/// the date-prefixed module name. The actual migration name on the
/// `seaql_migrations` table comes from
/// [`MigrationName::name`](sea_orm_migration::MigrationName::name), not
/// the type ident, so this alias is purely an ergonomic re-export.
pub use m_create_features_table::Migration as CreateFeaturesTable;
