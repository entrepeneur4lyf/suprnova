//! Create the `sessions` table consumed by
//! `suprnova::session::driver::DatabaseSessionDriver`.
//!
//! Schema mirrors the SeaORM entity defined in
//! `framework/src/session/driver/database.rs::sessions::Model`:
//!
//! - `id`           VARCHAR PK   — session ID (40-char alphanumeric)
//! - `user_id`      VARCHAR NULL — authenticated user id (string)
//! - `payload`      TEXT         — JSON-serialised session data map
//! - `csrf_token`   VARCHAR      — CSRF token for this session
//! - `last_activity` TIMESTAMP   — last access; used for expiry + GC
//!
//! Without this table the database session driver fails every request
//! the moment `SessionMiddleware` tries to read or write a session, so
//! it ships alongside the auth-dependent avatar upload dogfood.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Sessions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Sessions::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Sessions::UserId).string().null())
                    .col(ColumnDef::new(Sessions::Payload).text().not_null())
                    .col(ColumnDef::new(Sessions::CsrfToken).string().not_null())
                    .col(
                        ColumnDef::new(Sessions::LastActivity)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // GC and "find by user" both filter on these columns; index them
        // so the driver scales past the in-memory test fixture.
        manager
            .create_index(
                Index::create()
                    .name("idx_sessions_user_id")
                    .table(Sessions::Table)
                    .col(Sessions::UserId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_sessions_last_activity")
                    .table(Sessions::Table)
                    .col(Sessions::LastActivity)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Sessions::Table).to_owned())
            .await
    }
}

/// Table and column identifiers for sessions
#[derive(DeriveIden)]
enum Sessions {
    Table,
    Id,
    UserId,
    Payload,
    CsrfToken,
    LastActivity,
}
