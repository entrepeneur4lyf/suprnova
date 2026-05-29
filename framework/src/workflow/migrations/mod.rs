//! Framework-owned migrations for the durable workflow engine.
//!
//! These create the `workflows` and `workflow_steps` tables consumed
//! by [`crate::workflow::store`]. Consumer apps register them in their
//! own `Migrator`:
//!
//! ```ignore
//! use suprnova::workflow::migrations::{CreateWorkflowsTable, CreateWorkflowStepsTable};
//! use sea_orm_migration::MigratorTrait;
//!
//! pub struct Migrator;
//!
//! impl MigratorTrait for Migrator {
//!     fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
//!         vec![
//!             Box::new(CreateWorkflowsTable),
//!             Box::new(CreateWorkflowStepsTable),
//!         ]
//!     }
//! }
//! ```
//!
//! The framework owns the schema, the app owns when to apply it. New
//! Suprnova scaffolds wire these automatically via the `suprnova new`
//! template under `migrations/`; existing apps add the two `Box::new`
//! entries shown above.
//!
//! Matches the convention used by [`crate::features::migrations`] and
//! [`crate::payments::migrations`].

pub mod m_create_workflow_steps_table;
pub mod m_create_workflows_table;

/// Public alias so consumers can write `CreateWorkflowsTable` instead
/// of the date-prefixed module name. The actual migration name on the
/// `seaql_migrations` table comes from
/// [`MigrationName::name`](sea_orm_migration::MigrationName::name), not
/// the type ident.
pub use m_create_workflow_steps_table::Migration as CreateWorkflowStepsTable;
pub use m_create_workflows_table::Migration as CreateWorkflowsTable;
