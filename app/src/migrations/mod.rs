pub use sea_orm_migration::prelude::*;

mod m20251208_160100_create_users_table;
mod m20251208_200000_create_todos_table;
mod m20251208_220000_create_sessions_table;
mod m20251208_230000_create_remember_tokens_table;
mod m20251208_240000_create_posts_table;
mod m20251209_000000_create_auth_ceremony_tokens_table;
mod m_2026_05_19_phase_10a_user_columns;
mod m_2026_05_19_phase_10b_relations_schema;
mod m_2026_05_20_phase_10b_profiles;
mod m_2026_05_21_phase_10c_audit_log;

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
            // Phase 10A T11 — adds the columns the migrated dogfood
            // models (User, Todo) declare on top of the original
            // bare-bones schema. See the migration's module doc for
            // the per-column rationale.
            Box::new(m_2026_05_19_phase_10a_user_columns::Migration),
            // Phase 10B T10 — schema for the relations dogfood
            // (roles + role_user pivot, comments polymorphic, videos,
            // tags + taggables polymorphic m2m pivot). Listed last so
            // re-runs against existing dev databases pick it up as a
            // new pending migration.
            Box::new(m_2026_05_19_phase_10b_relations_schema::Migration),
            // Phase 10B P5 — `profiles` table for the HasOne dogfood
            // (User.profile: HasOne<Profile>). Closes the relation-kind
            // coverage gap from the Phase 10B closeout self-audit.
            // Listed last so re-runs against existing dev databases
            // pick it up as a new pending migration.
            Box::new(m_2026_05_20_phase_10b_profiles::Migration),
            // Phase 10C T14 — `audit_log` table for the transaction
            // dogfood. The closeout end-to-end tests wrap a user
            // creation alongside an audit row in a single
            // `DB::transaction` block to pin the rollback contract.
            Box::new(m_2026_05_21_phase_10c_audit_log::Migration),
            // Auth ceremony tokens — single-use OAuth state + Passkey
            // challenge storage (ChatGPT audit `torii_integration`
            // HIGH #3). Externalises the single-use authority from the
            // race-prone session R-M-W to a UNIQUE-selector table with
            // atomic conditional DELETE.
            Box::new(m20251209_000000_create_auth_ceremony_tokens_table::Migration),
        ]
    }
}
