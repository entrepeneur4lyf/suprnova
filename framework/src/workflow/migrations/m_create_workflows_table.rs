//! Migration that creates the framework-owned `workflows` table consumed
//! by [`crate::workflow::store`].
//!
//! Schema mirrors the CLI scaffolder template at
//! `suprnova-cli/src/templates/files/backend/migrations/create_workflows_table.rs.tpl`
//! — see [`crate::workflow::migrations`] for the convention.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    // Explicit, date-prefixed name. `DeriveMigrationName` would derive
    // from the parent module path (`m_create_workflows_table`), which
    // works today but offers no protection against later framework
    // migrations colliding on path. Matches the convention used by the
    // features and payments migrations.
    fn name(&self) -> &str {
        "m20260524_000001_create_workflows_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Workflows::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Workflows::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Workflows::Name).string().not_null())
                    .col(ColumnDef::new(Workflows::Status).string().not_null())
                    .col(ColumnDef::new(Workflows::Input).text().not_null())
                    .col(ColumnDef::new(Workflows::Output).text().null())
                    .col(ColumnDef::new(Workflows::Error).text().null())
                    .col(ColumnDef::new(Workflows::Attempts).integer().not_null())
                    .col(ColumnDef::new(Workflows::MaxAttempts).integer().not_null())
                    .col(ColumnDef::new(Workflows::NextRunAt).timestamp().null())
                    .col(ColumnDef::new(Workflows::LockedUntil).timestamp().null())
                    .col(ColumnDef::new(Workflows::WorkerId).string().null())
                    .col(
                        ColumnDef::new(Workflows::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Workflows::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(Workflows::StartedAt).timestamp().null())
                    .col(ColumnDef::new(Workflows::CompletedAt).timestamp().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_workflows_status")
                    .table(Workflows::Table)
                    .col(Workflows::Status)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_workflows_next_run_at")
                    .table(Workflows::Table)
                    .col(Workflows::NextRunAt)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_workflows_locked_until")
                    .table(Workflows::Table)
                    .col(Workflows::LockedUntil)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Workflows::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Workflows {
    Table,
    Id,
    Name,
    Status,
    Input,
    Output,
    Error,
    Attempts,
    MaxAttempts,
    NextRunAt,
    LockedUntil,
    WorkerId,
    CreatedAt,
    UpdatedAt,
    StartedAt,
    CompletedAt,
}
