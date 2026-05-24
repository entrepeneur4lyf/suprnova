//! Create the `remember_tokens` table consumed by
//! `suprnova::auth::remember` (codex review finding #13; selector
//! column added per ChatGPT audit `auth` HIGH #1 + #2).
//!
//! Schema mirrors the SeaORM entity defined in
//! `framework/src/auth/remember.rs::entity::Model`:
//!
//! - `id` BIGINT PK auto-increment
//! - `user_id` VARCHAR not null — opaque string id (post-Phase-3
//!   String-everywhere refactor; no FK on purpose, so the table is
//!   usable against any user store)
//! - `selector` VARCHAR not null UNIQUE — 22-char URL-safe base64
//!   lookup key for O(1) indexed verification (replaces the previous
//!   full-scan bcrypt design)
//! - `token_hash` VARCHAR not null — bcrypt hash of the verifier
//!   plaintext (the verifier half of the composite cookie token)
//! - `expires_at` TIMESTAMP not null — token TTL boundary
//! - `created_at` TIMESTAMP not null
//! - `last_used_at` TIMESTAMP null
//!
//! Three indexes:
//!
//! - `idx_remember_tokens_user_id` — revoke-by-user is a DELETE with
//!   `WHERE user_id = ?`; index makes it O(matches) instead of O(table).
//! - `idx_remember_tokens_expires_at` — `prune_expired` filters
//!   `WHERE expires_at <= now()`.
//! - `idx_remember_tokens_selector` — UNIQUE; verification does
//!   `SELECT ... WHERE selector = ? LIMIT 1`. The UNIQUE constraint
//!   also enforces selector collision impossibility at the DB level.

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

        manager
            .create_index(
                Index::create()
                    .name("idx_remember_tokens_user_id")
                    .table(RememberTokens::Table)
                    .col(RememberTokens::UserId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_remember_tokens_expires_at")
                    .table(RememberTokens::Table)
                    .col(RememberTokens::ExpiresAt)
                    .to_owned(),
            )
            .await?;

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

/// Table and column identifiers for remember_tokens.
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
