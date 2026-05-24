//! Create the `auth_ceremony_tokens` table consumed by
//! `suprnova::torii_integration::ceremony` (ChatGPT audit
//! `torii_integration` HIGH #3).
//!
//! Atomic single-use ceremony store for OAuth + Passkey auth flows.
//! Externalises the single-use authority from the session (where
//! get+forget is a non-atomic R-M-W) to this table where a
//! conditional DELETE keyed on (id, selector) gives exactly-once
//! consumption under concurrency.
//!
//! Schema mirrors the SeaORM entity at
//! `framework/src/torii_integration/ceremony.rs::entity::Model`:
//!
//! - `id`         BIGINT PK auto-increment
//! - `selector`   VARCHAR not null UNIQUE — the ceremony id
//! - `kind`       VARCHAR not null — discriminator ("oauth",
//!   "passkey_register", "passkey_authenticate")
//! - `payload`    TEXT not null — opaque JSON
//! - `expires_at` TIMESTAMP not null — token TTL boundary
//! - `created_at` TIMESTAMP not null
//!
//! Indexes:
//!
//! - `idx_auth_ceremony_tokens_selector` — UNIQUE, primary lookup
//! - `idx_auth_ceremony_tokens_expires_at` — for `prune_expired`

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AuthCeremonyTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AuthCeremonyTokens::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(AuthCeremonyTokens::Selector).string().not_null())
                    .col(ColumnDef::new(AuthCeremonyTokens::Kind).string().not_null())
                    .col(ColumnDef::new(AuthCeremonyTokens::Payload).text().not_null())
                    .col(ColumnDef::new(AuthCeremonyTokens::ExpiresAt).timestamp().not_null())
                    .col(
                        ColumnDef::new(AuthCeremonyTokens::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_auth_ceremony_tokens_selector")
                    .table(AuthCeremonyTokens::Table)
                    .col(AuthCeremonyTokens::Selector)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_auth_ceremony_tokens_expires_at")
                    .table(AuthCeremonyTokens::Table)
                    .col(AuthCeremonyTokens::ExpiresAt)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AuthCeremonyTokens::Table).to_owned())
            .await
    }
}

/// Table and column identifiers for auth_ceremony_tokens.
#[derive(DeriveIden)]
enum AuthCeremonyTokens {
    Table,
    Id,
    Selector,
    Kind,
    Payload,
    ExpiresAt,
    CreatedAt,
}
