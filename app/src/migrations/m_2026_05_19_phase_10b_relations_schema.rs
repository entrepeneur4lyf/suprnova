//! Phase 10B T10 — schema for the relations dogfood.
//!
//! Adds the tables every Phase 10B relation kind needs to exercise
//! against the example app's real `Migrator`. The new tables compose
//! with the existing `users` / `posts` / `todos` schema:
//!
//! - `roles` + `role_user` — User has_many_to_many Roles with an
//!   `assigned_at` pivot column. UNIQUE on `(user_id, role_id)` so
//!   the `attach` / `sync` semantics line up with Laravel's pivot
//!   constraints (every pair shows up at most once).
//! - `comments` — polymorphic via `commentable_id` + `commentable_type`.
//!   Single table, indexed on the pair, no FK constraints (matches
//!   Laravel's morph convention — type-string discrimination, not
//!   per-target tables).
//! - `videos` — second target for the morph relation, so the
//!   dogfood exercises both branches of `CommentableMorph::Post(...)`
//!   and `CommentableMorph::Video(...)`.
//! - `tags` + `taggables` — polymorphic m2m. UNIQUE on
//!   `(tag_id, taggable_id, taggable_type)` so the same tag is never
//!   attached twice to the same parent.
//!
//! Postgres-compatible SQL via SeaORM `Table::create`; SQLite friendly
//! (the dogfood test bed runs against `TestDatabase::fresh::<Migrator>()`).
//! `timestamp_with_time_zone` matches the rest of the Phase-10A migration
//! family — the framework's `chrono::DateTime<chrono::Utc>` deserializer
//! handles either flavour.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ---- roles + role_user pivot ------------------------------------
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("roles"))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("name")).string().not_null())
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
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("role_user"))
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
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("role_id"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("assigned_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
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
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_role_user_unique")
                    .table(Alias::new("role_user"))
                    .col(Alias::new("user_id"))
                    .col(Alias::new("role_id"))
                    .unique()
                    .to_owned(),
            )
            .await?;

        // ---- comments (polymorphic — Post + Video) ----------------------
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("comments"))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("commentable_id"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("commentable_type"))
                            .string()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("body")).text().not_null())
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
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_comments_commentable")
                    .table(Alias::new("comments"))
                    .col(Alias::new("commentable_id"))
                    .col(Alias::new("commentable_type"))
                    .to_owned(),
            )
            .await?;

        // ---- videos -----------------------------------------------------
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("videos"))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("url")).string().not_null())
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
            .await?;

        // ---- tags + taggables (polymorphic m2m pivot) -------------------
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("tags"))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("name"))
                            .string()
                            .not_null()
                            .unique_key(),
                    )
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
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("taggables"))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("tag_id"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("taggable_id"))
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("taggable_type"))
                            .string()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_taggables_unique")
                    .table(Alias::new("taggables"))
                    .col(Alias::new("tag_id"))
                    .col(Alias::new("taggable_id"))
                    .col(Alias::new("taggable_type"))
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for t in [
            "taggables",
            "tags",
            "videos",
            "comments",
            "role_user",
            "roles",
        ] {
            manager
                .drop_table(Table::drop().table(Alias::new(t)).to_owned())
                .await?;
        }
        Ok(())
    }
}
