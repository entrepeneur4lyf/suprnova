//! Migration that creates the framework-owned `workflow_steps` table
//! consumed by [`crate::workflow::store`] for per-step caching.
//!
//! Schema mirrors the CLI scaffolder template at
//! `suprnova-cli/src/templates/files/backend/migrations/create_workflow_steps_table.rs.tpl`
//! — see [`crate::workflow::migrations`] for the convention.

use sea_orm_migration::prelude::*;

/// Migration that creates the framework-owned `workflow_steps` table.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260524_000002_create_workflow_steps_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(WorkflowSteps::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WorkflowSteps::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(WorkflowSteps::WorkflowId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WorkflowSteps::StepIndex)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(WorkflowSteps::StepName).string().not_null())
                    .col(ColumnDef::new(WorkflowSteps::Status).string().not_null())
                    .col(ColumnDef::new(WorkflowSteps::Input).text().not_null())
                    .col(ColumnDef::new(WorkflowSteps::Output).text().null())
                    .col(ColumnDef::new(WorkflowSteps::Error).text().null())
                    .col(ColumnDef::new(WorkflowSteps::Attempts).integer().not_null())
                    .col(
                        ColumnDef::new(WorkflowSteps::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(WorkflowSteps::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(WorkflowSteps::StartedAt).timestamp().null())
                    .col(
                        ColumnDef::new(WorkflowSteps::CompletedAt)
                            .timestamp()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_workflow_steps_workflow_id")
                    .table(WorkflowSteps::Table)
                    .col(WorkflowSteps::WorkflowId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_workflow_steps_unique")
                    .table(WorkflowSteps::Table)
                    .col(WorkflowSteps::WorkflowId)
                    .col(WorkflowSteps::StepIndex)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(WorkflowSteps::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum WorkflowSteps {
    Table,
    Id,
    WorkflowId,
    StepIndex,
    StepName,
    Status,
    Input,
    Output,
    Error,
    Attempts,
    CreatedAt,
    UpdatedAt,
    StartedAt,
    CompletedAt,
}
