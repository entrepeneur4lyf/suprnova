//! Migration adding the `last_used_timestep` column to
//! `two_factor_credentials`. Consumed by
//! [`crate::auth_flows::TwoFactor::verify`] to reject TOTP code
//! replays within the 30-second timestep window.
//!
//! Lands separately from
//! [`super::migration::Migration`] (rather than editing the original
//! migration in place) so any deployment that already ran the v1
//! create-table migration can roll forward without dropping data.

use sea_orm_migration::prelude::*;

/// Migration that adds the `last_used_timestep` column to
/// `two_factor_credentials` for TOTP replay protection.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260101_000002_add_totp_replay_protection_to_two_factor_credentials"
    }
}

#[derive(DeriveIden)]
enum TwoFactorCredentials {
    Table,
    LastUsedTimestep,
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(TwoFactorCredentials::Table)
                    .add_column(
                        ColumnDef::new(TwoFactorCredentials::LastUsedTimestep)
                            .big_integer()
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
                    .table(TwoFactorCredentials::Table)
                    .drop_column(TwoFactorCredentials::LastUsedTimestep)
                    .to_owned(),
            )
            .await
    }
}
