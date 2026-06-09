//! Provider-agnostic single-use token store for email verification and
//! password reset. SeaORM-backed over `DB::connection()`, modeled on
//! `auth::remember` and `torii_integration::ceremony`. Tokens are stored
//! as a SHA-256 hash of a high-entropy plaintext; single-use is enforced
//! by an atomic `used_at` update.

use chrono::Duration;

/// What an `auth_flow_tokens` row authorizes. Stored as the stable
/// lowercase string in the `purpose` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenPurpose {
    /// Authorizes confirming ownership of an email address.
    EmailVerification,
    /// Authorizes setting a new password after a forgot-password request.
    PasswordReset,
}

impl TokenPurpose {
    /// The stable lowercase string stored in the `purpose` column.
    pub fn as_str(self) -> &'static str {
        match self {
            TokenPurpose::EmailVerification => "email_verification",
            TokenPurpose::PasswordReset => "password_reset",
        }
    }

    /// Default lifetime: 24h for verification, 15m for reset (matching
    /// torii's prior defaults).
    pub fn default_ttl(self) -> Duration {
        match self {
            TokenPurpose::EmailVerification => Duration::hours(24),
            TokenPurpose::PasswordReset => Duration::minutes(15),
        }
    }
}

/// Returns the SeaORM `TableCreateStatement` for `auth_flow_tokens`.
/// Migrations (framework + scaffold) call this. Modeled on the
/// `two_factor` migration's column-builder body, but returns the
/// statement directly so the caller decides when/where to apply it.
///
/// Column shape mirrors the [`entity`] module:
///
/// - `id`         BIGINT PK auto-increment — matches `Model::id: i64`
/// - `user_id`    TEXT not null — opaque string id (String-everywhere)
/// - `token_hash` TEXT not null — SHA-256 hash of the plaintext token
/// - `purpose`    TEXT not null — [`TokenPurpose::as_str`] discriminator
/// - `expires_at` TIMESTAMP not null — token TTL boundary
/// - `used_at`    TIMESTAMP null — set atomically on single-use consume
/// - `created_at` TIMESTAMP not null
///
/// Timestamps are plain `.timestamp()` (not `timestamp_with_time_zone`)
/// to pair with the entity's `chrono::NaiveDateTime` fields, which are
/// written via `.naive_utc()` — the same convention `auth::remember` and
/// `torii_integration::ceremony` use.
pub fn create_auth_flow_tokens_table() -> sea_orm::sea_query::TableCreateStatement {
    use sea_orm::sea_query::{ColumnDef, Table};

    Table::create()
        .table(AuthFlowTokens::Table)
        .if_not_exists()
        .col(
            ColumnDef::new(AuthFlowTokens::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(ColumnDef::new(AuthFlowTokens::UserId).text().not_null())
        .col(ColumnDef::new(AuthFlowTokens::TokenHash).text().not_null())
        .col(ColumnDef::new(AuthFlowTokens::Purpose).text().not_null())
        .col(
            ColumnDef::new(AuthFlowTokens::ExpiresAt)
                .timestamp()
                .not_null(),
        )
        .col(ColumnDef::new(AuthFlowTokens::UsedAt).timestamp().null())
        .col(
            ColumnDef::new(AuthFlowTokens::CreatedAt)
                .timestamp()
                .not_null(),
        )
        .to_owned()
}

/// Column identifiers for the `auth_flow_tokens` table builder. Kept in
/// sync with [`entity::Model`].
#[derive(sea_orm::DeriveIden)]
enum AuthFlowTokens {
    Table,
    Id,
    UserId,
    TokenHash,
    Purpose,
    ExpiresAt,
    UsedAt,
    CreatedAt,
}

/// SeaORM entity for the `auth_flow_tokens` table.
///
/// Schema (kept in sync with [`create_auth_flow_tokens_table`]):
///
/// - `id`         BIGINT PK auto-increment
/// - `user_id`    TEXT not null — opaque string id
/// - `token_hash` TEXT not null — SHA-256 hash of the plaintext token
/// - `purpose`    TEXT not null — [`TokenPurpose::as_str`] discriminator
/// - `expires_at` TIMESTAMP not null — token TTL boundary
/// - `used_at`    TIMESTAMP null — set on single-use consume (Task 2)
/// - `created_at` TIMESTAMP not null
pub mod entity {
    use sea_orm::entity::prelude::*;

    /// SeaORM model for a single row in `auth_flow_tokens`.
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "auth_flow_tokens")]
    pub struct Model {
        /// Auto-increment primary key.
        #[sea_orm(primary_key)]
        pub id: i64,
        /// Opaque string id of the user the token authorizes.
        pub user_id: String,
        /// SHA-256 hash of the high-entropy plaintext token.
        pub token_hash: String,
        /// What the row authorizes; the stable string from [`super::TokenPurpose::as_str`].
        pub purpose: String,
        /// TTL boundary; the token is rejected once `now > expires_at`.
        pub expires_at: chrono::NaiveDateTime,
        /// Set atomically when the token is consumed; single-use is enforced by this column (Task 2).
        pub used_at: Option<chrono::NaiveDateTime>,
        /// Wall-clock time the token row was created.
        pub created_at: chrono::NaiveDateTime,
    }

    /// SeaORM relation enum — `auth_flow_tokens` is a leaf table with no
    /// declared foreign-key relations.
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purpose_strings_are_stable() {
        assert_eq!(
            TokenPurpose::EmailVerification.as_str(),
            "email_verification"
        );
        assert_eq!(TokenPurpose::PasswordReset.as_str(), "password_reset");
    }

    #[test]
    fn default_ttls_match_prior_torii_windows() {
        assert_eq!(
            TokenPurpose::EmailVerification.default_ttl(),
            Duration::hours(24)
        );
        assert_eq!(
            TokenPurpose::PasswordReset.default_ttl(),
            Duration::minutes(15)
        );
    }
}
