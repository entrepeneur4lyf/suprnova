//! Migration that creates the `two_factor_credentials` table consumed
//! by [`crate::auth_flows::TwoFactor`].
//!
//! Consumer apps include this migration in their `Migrator`'s
//! `migrations()` list — the framework owns the schema, the app owns
//! when to apply it. The example app wires this in Phase 11 Task 9.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    // Explicit, file-stable name. `DeriveMigrationName` derives from
    // the parent module path, which produces just `migration` here —
    // not unique enough for the `seaql_migrations` table once a
    // second framework-owned migration lands in another module with
    // the same file name. The date prefix matches the convention the
    // example app uses for its own migrations.
    fn name(&self) -> &str {
        "m20260101_000001_create_two_factor_credentials"
    }
}

#[derive(DeriveIden)]
enum TwoFactorCredentials {
    Table,
    UserId,
    Secret,
    ConfirmedAt,
    RecoveryCodes,
    CreatedAt,
    UpdatedAt,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(TwoFactorCredentials::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TwoFactorCredentials::UserId)
                            .text()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(TwoFactorCredentials::Secret)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TwoFactorCredentials::ConfirmedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(TwoFactorCredentials::RecoveryCodes)
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(TwoFactorCredentials::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(TwoFactorCredentials::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TwoFactorCredentials::Table).to_owned())
            .await
    }
}
