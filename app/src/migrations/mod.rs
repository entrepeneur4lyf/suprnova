pub use sea_orm_migration::prelude::*;

mod m20251208_160100_create_users_table;
mod m20251208_200000_create_todos_table;
mod m20251208_220000_create_sessions_table;
mod m20251208_230000_create_remember_tokens_table;
mod m20251208_240000_create_posts_table;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20251208_160100_create_users_table::Migration),
            Box::new(m20251208_200000_create_todos_table::Migration),
            Box::new(m20251208_220000_create_sessions_table::Migration),
            Box::new(m20251208_230000_create_remember_tokens_table::Migration),
            Box::new(m20251208_240000_create_posts_table::Migration),
            // Phase 11 — framework-owned 2FA credentials table. The
            // framework ships the migration; the app's `Migrator`
            // just lists it so `suprnova migrate` provisions
            // `two_factor_credentials` alongside this project's own
            // schema. Listed last so re-runs against existing dev
            // databases pick it up as a new pending migration.
            Box::new(suprnova::auth_flows::two_factor::migration::Migration),
            // Phase 11 R4 — adds last_used_timestep to
            // two_factor_credentials for TOTP replay protection.
            Box::new(suprnova::auth_flows::two_factor::migration_replay::Migration),
            // Phase 13 — framework-owned features table. Powers
            // DatabaseEvaluator + admin CRUD. The app's Migrator
            // includes it so `suprnova migrate` provisions the table
            // alongside this project's own schema.
            Box::new(suprnova::features::migrations::CreateFeaturesTable),
        ]
    }
}
