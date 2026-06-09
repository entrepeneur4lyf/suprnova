//! Provider-agnostic single-use token store. Powers email-verification
//! and password-reset links without binding either flow to a particular
//! mailer or user backend.
//!
//! - [`TokenPurpose`] — what a row authorizes (`email_verification` /
//!   `password_reset`), plus its stable string and default TTL.
//! - [`TokenStore`] — `issue` / `check` / `consume` / `prune_expired`
//!   over the `auth_flow_tokens` table.
//! - [`entity`] + [`create_auth_flow_tokens_table`] — the SeaORM entity
//!   and schema builder migrations apply.
//!
//! Tokens carry 256 bits of OS entropy; only their SHA-256 hash is
//! stored, and single-use is enforced at the database level by an atomic
//! conditional `used_at` UPDATE (the UPDATE's `rows_affected` is the
//! single-use authority, so consume is race-safe under concurrency).

use chrono::{Duration, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::database::DB;
use crate::error::FrameworkError;

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
/// - `token_hash` TEXT not null UNIQUE — SHA-256 hash of the plaintext
///   token; the UNIQUE constraint gives `check`/`consume` an indexed
///   equality lookup and backs the single-use guarantee at the DB level
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
        .col(
            ColumnDef::new(AuthFlowTokens::TokenHash)
                .text()
                .not_null()
                .unique_key(),
        )
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

/// Generate a fresh 256-bit high-entropy token plaintext, URL-safe
/// base64 (43 chars, no padding) — the value handed to the user inside
/// a verification / reset link. Mirrors `auth::remember`'s verifier:
/// `getrandom::fill` (direct OS RNG) over 32 bytes, then
/// `URL_SAFE_NO_PAD`. No new RNG dependency.
fn generate_plaintext() -> Result<String, FrameworkError> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| FrameworkError::internal(format!("OS RNG failure (auth-flow token): {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// SHA-256-hex of the plaintext token — what we store in `token_hash`.
///
/// These tokens carry 256 bits of OS entropy, so a fast cryptographic
/// hash is the correct choice (unlike low-entropy passwords, which want
/// a slow KDF). Reuses the framework's existing `sha2` digest + manual
/// hex idiom (`idempotency::hashed`); no new dependency.
fn hash_token(plaintext: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(plaintext.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Provider-agnostic single-use token store over the `auth_flow_tokens`
/// table. Powers email-verification and password-reset links without
/// binding either flow to a particular mailer or user backend.
///
/// The plaintext is high-entropy and lives only in the link handed to
/// the user; the DB stores its SHA-256 hash. Single-use is enforced at
/// the database level by an atomic conditional UPDATE of `used_at`.
pub struct TokenStore;

impl TokenStore {
    /// Mint a token for `user_id`, store its SHA-256 hash with `purpose`
    /// + expiry, and return the plaintext to embed in the link.
    ///
    /// `ttl` is added to the current time to compute `expires_at`; a
    /// non-positive `ttl` yields an already-expired row (useful for
    /// tests and a harmless no-op in production).
    pub async fn issue(
        user_id: &str,
        purpose: TokenPurpose,
        ttl: Duration,
    ) -> Result<String, FrameworkError> {
        let plaintext = generate_plaintext()?;
        let token_hash = hash_token(&plaintext);
        let now = Utc::now().naive_utc();
        let expires_at = now + ttl;

        let conn = DB::connection()?;
        let model = entity::ActiveModel {
            user_id: Set(user_id.to_string()),
            token_hash: Set(token_hash),
            purpose: Set(purpose.as_str().to_string()),
            expires_at: Set(expires_at),
            used_at: Set(None),
            created_at: Set(now),
            ..Default::default()
        };

        entity::Entity::insert(model)
            .exec(conn.inner())
            .await
            .map_err(|e| FrameworkError::database(format!("issue auth-flow token: {e}")))?;

        Ok(plaintext)
    }

    /// True if a live, unused token of `purpose` matches `token` —
    /// non-consuming. The `token_hash` column is UNIQUE, so this is an
    /// indexed equality lookup returning 0 or 1 rows.
    pub async fn check(token: &str, purpose: TokenPurpose) -> Result<bool, FrameworkError> {
        let conn = DB::connection()?;
        let now = Utc::now().naive_utc();
        let token_hash = hash_token(token);

        let row = entity::Entity::find()
            .filter(entity::Column::TokenHash.eq(token_hash))
            .filter(entity::Column::Purpose.eq(purpose.as_str()))
            .filter(entity::Column::ExpiresAt.gt(now))
            .filter(entity::Column::UsedAt.is_null())
            .one(conn.inner())
            .await
            .map_err(|e| FrameworkError::database(format!("check auth-flow token: {e}")))?;

        Ok(row.is_some())
    }

    /// Atomically consume the token: if a live, unused token of `purpose`
    /// matches, stamp `used_at` and return its `user_id`; otherwise
    /// `None`.
    ///
    /// Single-use is race-safe because the authority is the conditional
    /// UPDATE's `rows_affected`, not a separate read. The UPDATE's WHERE
    /// includes `used_at IS NULL AND expires_at > now AND purpose = …`,
    /// so exactly one of N concurrent consumers flips the row (sees
    /// `rows_affected == 1`); the rest match zero rows and get `None`.
    /// The `user_id` is read from the same UNIQUE `token_hash`, so the
    /// lookup is unambiguous.
    pub async fn consume(
        token: &str,
        purpose: TokenPurpose,
    ) -> Result<Option<String>, FrameworkError> {
        let conn = DB::connection()?;
        let now = Utc::now().naive_utc();
        let token_hash = hash_token(token);

        // `token_hash` is UNIQUE: this resolves the exactly-one candidate
        // row (if any) so we can return its `user_id`. The atomic UPDATE
        // below — not this read — is the single-use authority, so a
        // concurrent consumer that wins the UPDATE race is still rejected
        // here via `rows_affected`.
        let row = entity::Entity::find()
            .filter(entity::Column::TokenHash.eq(&token_hash))
            .filter(entity::Column::Purpose.eq(purpose.as_str()))
            .filter(entity::Column::ExpiresAt.gt(now))
            .filter(entity::Column::UsedAt.is_null())
            .one(conn.inner())
            .await
            .map_err(|e| {
                FrameworkError::database(format!("consume auth-flow token lookup: {e}"))
            })?;

        let row = match row {
            Some(r) => r,
            None => return Ok(None),
        };

        // Atomic conditional UPDATE: the `used_at IS NULL` predicate makes
        // this single-use under concurrency. Only one racer flips the row
        // (rows_affected == 1); the rest affect zero rows and bail.
        let update = entity::Entity::update_many()
            .col_expr(entity::Column::UsedAt, Expr::value(now))
            .filter(entity::Column::TokenHash.eq(&token_hash))
            .filter(entity::Column::Purpose.eq(purpose.as_str()))
            .filter(entity::Column::ExpiresAt.gt(now))
            .filter(entity::Column::UsedAt.is_null())
            .exec(conn.inner())
            .await
            .map_err(|e| FrameworkError::database(format!("consume auth-flow token: {e}")))?;

        if update.rows_affected != 1 {
            // Lost the consume race (or the row expired between the read
            // and the UPDATE). Treat as "already consumed / invalid".
            return Ok(None);
        }

        Ok(Some(row.user_id))
    }

    /// Delete every row whose `expires_at` is in the past. Returns the
    /// number of rows removed.
    ///
    /// Wire to a scheduled task (see `framework/src/schedule/`) so the
    /// table does not accumulate dead rows.
    pub async fn prune_expired() -> Result<u64, FrameworkError> {
        let conn = DB::connection()?;
        let now = Utc::now().naive_utc();
        let result = entity::Entity::delete_many()
            .filter(entity::Column::ExpiresAt.lt(now))
            .exec(conn.inner())
            .await
            .map_err(|e| FrameworkError::database(format!("prune auth-flow tokens: {e}")))?;
        Ok(result.rows_affected)
    }
}

/// SeaORM entity for the `auth_flow_tokens` table.
///
/// Schema (kept in sync with [`create_auth_flow_tokens_table`]):
///
/// - `id`         BIGINT PK auto-increment
/// - `user_id`    TEXT not null — opaque string id
/// - `token_hash` TEXT not null UNIQUE — SHA-256 hash of the plaintext token
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
        #[sea_orm(unique)]
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

    /// Fresh in-memory SQLite with the `auth_flow_tokens` table created
    /// and bound as the framework `DB` connection (thread-local via
    /// `TestContainer`, so each `#[tokio::test]` is isolated — no global
    /// lock or `#[serial]` needed). Hold the returned guard for the whole
    /// test or `DB::connection()` loses its binding.
    async fn test_db_with_auth_flow_tokens() -> crate::testing::TestDatabase {
        use sea_orm::ConnectionTrait;
        let db = crate::testing::TestDatabase::sqlite_memory()
            .await
            .expect("sqlite_memory");
        let conn = db.conn();
        let stmt = create_auth_flow_tokens_table();
        conn.execute(conn.get_database_backend().build(&stmt))
            .await
            .expect("create auth_flow_tokens table");
        db
    }

    #[tokio::test]
    async fn issue_then_consume_is_single_use() {
        let _db = test_db_with_auth_flow_tokens().await;
        let plaintext =
            TokenStore::issue("42", TokenPurpose::EmailVerification, Duration::hours(1))
                .await
                .unwrap();
        assert!(
            TokenStore::check(&plaintext, TokenPurpose::EmailVerification)
                .await
                .unwrap()
        );
        assert_eq!(
            TokenStore::consume(&plaintext, TokenPurpose::EmailVerification)
                .await
                .unwrap(),
            Some("42".to_string())
        );
        // After consume, the token is spent: check is false and a second
        // consume returns None (single-use enforced by the DB).
        assert!(
            !TokenStore::check(&plaintext, TokenPurpose::EmailVerification)
                .await
                .unwrap()
        );
        assert_eq!(
            TokenStore::consume(&plaintext, TokenPurpose::EmailVerification)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn wrong_purpose_and_expired_do_not_consume() {
        let _db = test_db_with_auth_flow_tokens().await;
        let t = TokenStore::issue("7", TokenPurpose::PasswordReset, Duration::hours(1))
            .await
            .unwrap();
        // A live token must not consume under the wrong purpose.
        assert_eq!(
            TokenStore::consume(&t, TokenPurpose::EmailVerification)
                .await
                .unwrap(),
            None
        );
        // ...and a non-consuming check under the wrong purpose is false.
        assert!(
            !TokenStore::check(&t, TokenPurpose::EmailVerification)
                .await
                .unwrap()
        );
        // The correct purpose still works (the wrong-purpose attempt did
        // not spend it).
        assert!(
            TokenStore::check(&t, TokenPurpose::PasswordReset)
                .await
                .unwrap()
        );

        // An already-expired token (negative TTL) never consumes.
        let expired = TokenStore::issue("7", TokenPurpose::PasswordReset, Duration::seconds(-1))
            .await
            .unwrap();
        assert!(
            !TokenStore::check(&expired, TokenPurpose::PasswordReset)
                .await
                .unwrap()
        );
        assert_eq!(
            TokenStore::consume(&expired, TokenPurpose::PasswordReset)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn prune_expired_removes_only_expired_rows() {
        let _db = test_db_with_auth_flow_tokens().await;
        let live = TokenStore::issue("1", TokenPurpose::EmailVerification, Duration::hours(1))
            .await
            .unwrap();
        let _expired =
            TokenStore::issue("2", TokenPurpose::EmailVerification, Duration::seconds(-1))
                .await
                .unwrap();

        let removed = TokenStore::prune_expired().await.unwrap();
        assert_eq!(removed, 1, "exactly the one expired row is pruned");
        // The live token survives the prune.
        assert!(
            TokenStore::check(&live, TokenPurpose::EmailVerification)
                .await
                .unwrap()
        );
    }
}
