//! SeaORM entity for the framework-owned `two_factor_credentials` table.
//!
//! Holds per-user TOTP secrets and recovery codes — encrypted at rest
//! via [`crate::crypto::Crypt`]. The `user_id` is opaque (any stringy
//! identifier the application uses, typically `torii::UserId.to_string()`)
//! and intentionally has no FK constraint so the schema is decoupled
//! from whichever user-storage backend the consuming app picks.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "two_factor_credentials")]
pub struct Model {
    /// Opaque per-user identifier (e.g. `torii::UserId.to_string()`).
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: String,
    /// `Crypt::encrypt_string`-encoded base32 TOTP secret.
    #[sea_orm(column_type = "Text")]
    pub secret: String,
    /// Set once the user proves possession of the authenticator
    /// device by submitting a valid TOTP code via
    /// [`crate::auth_flows::TwoFactor::confirm`]. Until non-NULL,
    /// [`crate::auth_flows::TwoFactor::is_enabled`] reports false and
    /// [`crate::auth_flows::TwoFactor::verify`] short-circuits to
    /// `Ok(false)`.
    pub confirmed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `Crypt::encrypt_string` of newline-joined plaintext recovery
    /// codes. `None` once every code has been consumed.
    #[sea_orm(column_type = "Text", nullable)]
    pub recovery_codes: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
