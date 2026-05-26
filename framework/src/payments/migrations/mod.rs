//! Framework-owned migrations for the payments subsystem.
//!
//! Re-exported under [`crate::payments::migrations`] so consumer apps
//! can register the schema in their own `Migrator`:
//!
//! ```ignore
//! use suprnova::payments::migrations::CreatePaymentsTables;
//!
//! impl sea_orm_migration::MigratorTrait for Migrator {
//!     fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
//!         vec![Box::new(CreatePaymentsTables)]
//!     }
//! }
//! ```

pub mod m_2026_05_22_000001_create_payments_tables;

use sea_orm_migration::MigrationTrait;

/// Public alias so consumers can write `CreatePaymentsTables` instead of
/// the date-prefixed module name. The actual migration name recorded in
/// the `seaql_migrations` table comes from `DeriveMigrationName`, which
/// derives from the module path — unique and stable.
pub use m_2026_05_22_000001_create_payments_tables::Migration as CreatePaymentsTables;

/// Returns all payments migrations in order. Wire into your app's
/// `Migrator::migrations()` to apply the payments schema.
pub fn migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![Box::new(
        m_2026_05_22_000001_create_payments_tables::Migration,
    )]
}
