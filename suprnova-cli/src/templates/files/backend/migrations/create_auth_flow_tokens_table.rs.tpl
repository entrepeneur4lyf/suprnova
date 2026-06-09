use sea_orm_migration::prelude::*;

/// Creates the `auth_flow_tokens` table that backs single-use email-
/// verification and password-reset links. The framework owns the schema —
/// this migration just applies the table builder it exposes so the columns,
/// indexes, and UNIQUE constraints stay in lockstep with the framework's
/// token store.
pub struct Migration;

impl MigrationName for Migration {
    // Explicit, file-stable name. `DeriveMigrationName` would derive from the
    // module path; an explicit string keeps the `seaql_migrations` ledger
    // stable and matches the date-prefixed convention the other migrations use.
    fn name(&self) -> &str {
        "m20240101_000004_create_auth_flow_tokens_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // The framework hands back a ready-to-apply `TableCreateStatement`
        // (`if_not_exists`, columns, UNIQUE on `token_hash`). Applying it here
        // keeps the app's schema in sync with the token store without
        // re-declaring the columns by hand.
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
