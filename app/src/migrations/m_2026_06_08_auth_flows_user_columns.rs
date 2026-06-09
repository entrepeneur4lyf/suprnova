//! Auth-flows dogfood — add `email_verified_at` to `users`.
//!
//! Tasks 5–8 of the provider-agnostic auth-flows change made the
//! `EmailVerification` / `PasswordReset` facades resolve through the
//! configured `UserProvider`. For the example app's flows to be
//! runtime-functional, the `User` model (now `MustVerifyEmail` +
//! `CanResetPassword`) carries a nullable `email_verified_at` column that the
//! `EloquentUserProvider<User>` reads in `is_email_verified` and stamps in
//! `mark_email_verified`.
//!
//! Additive on purpose, matching the established dogfood idiom: the earlier
//! `m20251208_*` / `m_2026_05_*` migrations stay frozen; this one runs last so
//! a dev DB with rows already in `users` picks the column up as a new pending
//! migration. `NULL` (the column default) means unverified, so existing rows
//! backfill correctly.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("users"))
                    .add_column(
                        ColumnDef::new(Alias::new("email_verified_at"))
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("users"))
                    .drop_column(Alias::new("email_verified_at"))
                    .to_owned(),
            )
            .await
    }
}
