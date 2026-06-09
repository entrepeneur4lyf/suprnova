//! Auth-flows dogfood — `auth_flow_tokens` single-use token table.
//!
//! Backs the email-verification and password-reset links. The framework owns
//! the schema and exposes a ready-to-apply `TableCreateStatement` via
//! [`suprnova::auth_flows::token_store::create_auth_flow_tokens_table`]; this
//! migration just applies it so the app's `Migrator` provisions the table
//! (columns + the UNIQUE `token_hash` index that backs single-use) alongside
//! the rest of the dogfood schema.
//!
//! Listed last in `mod.rs` so re-runs against an existing dev database pick it
//! up as a new pending migration.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    // Explicit, file-stable name. The framework's table builder lives in a
    // shared module, so an explicit string keeps the `seaql_migrations` ledger
    // unambiguous and matches the date-prefixed convention used elsewhere.
    fn name(&self) -> &str {
        "m_2026_06_08_auth_flow_tokens"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(suprnova::auth_flows::token_store::create_auth_flow_tokens_table())
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("auth_flow_tokens"))
                    .to_owned(),
            )
            .await
    }
}
