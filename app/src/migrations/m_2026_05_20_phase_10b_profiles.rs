//! Phase 10B P5 — schema for the HasOne dogfood.
//!
//! Adds a `profiles` table so `User.profile: HasOne<Profile>` can be
//! exercised end-to-end against the example app's real `Migrator`.
//!
//! Schema notes:
//!
//! - `user_id` is the FK back to `users`. The default Eloquent
//!   convention for `HasOne` is `<snake(parent)>_id`, so the column
//!   name matches `default_has_fk("User") == "user_id"` without
//!   requiring a `fk = "..."` override on the relation declaration.
//! - The column is `UNIQUE` — a User has AT MOST one Profile, by
//!   definition of HasOne. The unique key turns "violated invariant"
//!   into a constraint error rather than letting two profiles per
//!   user slip through.
//! - `timestamp_with_time_zone` with `Expr::current_timestamp()`
//!   defaults match the Phase 10B `relations_schema` migration's
//!   pattern. The model carries `timestamps` so the framework writes
//!   the columns explicitly; the schema-level default is the safety
//!   net for any direct SQL path that bypasses the model layer
//!   (e.g. seed scripts).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("profiles"))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("user_id"))
                            .big_integer()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Alias::new("bio")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
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
            .drop_table(Table::drop().table(Alias::new("profiles")).to_owned())
            .await
    }
}
