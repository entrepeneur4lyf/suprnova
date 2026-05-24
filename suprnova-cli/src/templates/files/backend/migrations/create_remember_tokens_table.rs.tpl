use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RememberTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RememberTokens::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(RememberTokens::UserId).string().not_null())
                    .col(ColumnDef::new(RememberTokens::Selector).string().not_null())
                    .col(ColumnDef::new(RememberTokens::TokenHash).string().not_null())
                    .col(ColumnDef::new(RememberTokens::ExpiresAt).timestamp().not_null())
                    .col(
                        ColumnDef::new(RememberTokens::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(RememberTokens::LastUsedAt).timestamp().null())
                    .to_owned(),
            )
            .await?;

        // Revoke-by-user (DELETE WHERE user_id = ?) and "log out
        // everywhere" filter on this column.
        manager
            .create_index(
                Index::create()
                    .name("idx_remember_tokens_user_id")
                    .table(RememberTokens::Table)
                    .col(RememberTokens::UserId)
                    .to_owned(),
            )
            .await?;

        // prune_expired filters `expires_at <= now()`.
        manager
            .create_index(
                Index::create()
                    .name("idx_remember_tokens_expires_at")
                    .table(RememberTokens::Table)
                    .col(RememberTokens::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        // verify_and_rotate looks up by selector — UNIQUE + indexed
        // gives O(1) lookup and enforces selector collision impossibility
        // at the DB level.
        manager
            .create_index(
                Index::create()
                    .name("idx_remember_tokens_selector")
                    .table(RememberTokens::Table)
                    .col(RememberTokens::Selector)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RememberTokens::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum RememberTokens {
    Table,
    Id,
    UserId,
    Selector,
    TokenHash,
    ExpiresAt,
    CreatedAt,
    LastUsedAt,
}
