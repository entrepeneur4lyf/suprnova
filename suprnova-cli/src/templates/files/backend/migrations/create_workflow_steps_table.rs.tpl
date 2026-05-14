use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

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
                    .col(ColumnDef::new(WorkflowSteps::WorkflowId).big_integer().not_null())
                    .col(ColumnDef::new(WorkflowSteps::StepIndex).integer().not_null())
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
                    .col(ColumnDef::new(WorkflowSteps::CompletedAt).timestamp().null())
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_workflow_steps_workflow_id")
                    .table(WorkflowSteps::Table)
                    .col(WorkflowSteps::WorkflowId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
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
