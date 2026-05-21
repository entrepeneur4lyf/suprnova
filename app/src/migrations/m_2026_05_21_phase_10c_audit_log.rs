//! Phase 10C T14 — `audit_log` table for the transaction dogfood.
//!
//! The closeout dogfood demonstrates `DB::transaction` by wrapping a
//! user creation alongside an `audit_log` insert in a single
//! transaction. If the transaction body returns an error after the
//! audit row is written, the rollback must un-write it — that's the
//! pin the test exercises.
//!
//! Tiny denormalised schema on purpose: this is a dogfood
//! demonstration, not the real audit-trail surface a production app
//! would build. `event` is a string discriminator, `actor_id` is an
//! optional FK back to `users` (NULL when the actor is the system),
//! `payload` is a free-form text blob (JSON if the caller wants to
//! structure it). Real apps reach for a Phase 8-shaped admin table
//! design — this table just proves the transactional path holds.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("audit_log"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("event"))
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("actor_id"))
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("payload"))
                            .text()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
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
            .drop_table(Table::drop().table(Alias::new("audit_log")).to_owned())
            .await
    }
}
