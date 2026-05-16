use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Posts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Posts::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // The author_id matches the column name used by
                    // app/src/policies/post_policy.rs and the existing
                    // dogfood references; we keep it i32 to match the
                    // users.id pk type and avoid a casting layer on the
                    // PostPolicy `post.author_id == user.id` comparison.
                    .col(ColumnDef::new(Posts::AuthorId).integer().not_null())
                    .col(ColumnDef::new(Posts::Title).string().not_null())
                    .col(ColumnDef::new(Posts::Body).text().not_null())
                    .col(
                        ColumnDef::new(Posts::IsPublic)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Posts::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Posts::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Posts::Table).to_owned())
            .await
    }
}

/// Table and column identifiers for posts
#[derive(DeriveIden)]
enum Posts {
    Table,
    Id,
    AuthorId,
    Title,
    Body,
    IsPublic,
    CreatedAt,
    UpdatedAt,
}
