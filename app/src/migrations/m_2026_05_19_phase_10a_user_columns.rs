//! Phase 10A T11 — extend `users` (and `todos`) with the columns the
//! migrated `#[suprnova::model]` versions of the dogfood entities
//! declare.
//!
//! The original `users` migration shipped a deliberately-sparse table
//! (id + timestamps) because the dogfood didn't carry any real identity
//! attributes at the time. The Phase 10A T11 migration turns the
//! example app's `User` into a proper Eloquent dogfood by adding the
//! columns Laravel users expect (`name`, `email`, `password`,
//! `remember_token`) plus the two columns that exercise the new
//! framework surface: `active` (cast via `AsBool`) and `deleted_at`
//! (soft-delete tombstone, cast via `AsOptionalDateTime`).
//!
//! Same story for `todos`: the macro version adds a `done: bool`
//! field with an `AsBool` cast so the dogfood demonstrates the
//! primitive cast path on a non-trivial real model.
//!
//! Additive migration on purpose. The existing `m20251208_*` migrations
//! stay frozen; this migration runs last so a dev DB with rows already
//! in `users` / `todos` picks up the new columns without rebuilding.
//! `DEFAULT` clauses cover backfilling existing rows.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ---- users: identity attributes ---------------------------
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("users"))
                    .add_column(
                        ColumnDef::new(Alias::new("name"))
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("users"))
                    .add_column(
                        ColumnDef::new(Alias::new("email"))
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("users"))
                    .add_column(
                        ColumnDef::new(Alias::new("password"))
                            .string()
                            .not_null()
                            .default(""),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("users"))
                    .add_column(
                        ColumnDef::new(Alias::new("remember_token"))
                            .string()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("users"))
                    .add_column(
                        ColumnDef::new(Alias::new("active"))
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("users"))
                    .add_column(
                        ColumnDef::new(Alias::new("deleted_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        // ---- todos: done flag for AsBool cast dogfood -------------
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("todos"))
                    .add_column(
                        ColumnDef::new(Alias::new("done"))
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("users"))
                    .drop_column(Alias::new("name"))
                    .drop_column(Alias::new("email"))
                    .drop_column(Alias::new("password"))
                    .drop_column(Alias::new("remember_token"))
                    .drop_column(Alias::new("active"))
                    .drop_column(Alias::new("deleted_at"))
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("todos"))
                    .drop_column(Alias::new("done"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}
