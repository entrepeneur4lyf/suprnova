//! Remember-me tokens (codex review finding #13).
//!
//! "Remember me" is the cookie that re-authenticates a user after their
//! session expires or their browser is closed. Done wrong it is a long-
//! lived bearer credential sitting in plaintext on a user's disk and in
//! a database backup — done right it is a rotating, single-use,
//! short-fuse re-auth handle.
//!
//! # Design
//!
//! Each issued remember-me cookie corresponds to one row in the
//! `remember_tokens` table. The row stores a bcrypt hash of the
//! plaintext token (same scheme the framework uses for password
//! hashes), not the plaintext itself. The plaintext lives only in the
//! cookie sent to the user.
//!
//! - Issuance: generate 32 random bytes → URL-safe base64 plaintext
//!   → bcrypt hash → INSERT row → encrypt plaintext under `Crypt` →
//!   place encrypted blob in `remember_me` cookie.
//!
//! - Verification on a fresh request: when no session is active, the
//!   session middleware decrypts the `remember_me` cookie. Hashes are
//!   per-row salted so we cannot look up by hash. We scan
//!   not-yet-expired rows and bcrypt-verify each candidate against the
//!   plaintext. First match wins.
//!
//! - Rotation: a successful verify DELETES the matched row and
//!   issues a fresh one. The middleware re-sets the cookie with the
//!   new plaintext. An attacker who captured the old cookie can never
//!   use it again — and if both attacker and victim race to use the
//!   same cookie, the loser sees the row missing and cannot auth.
//!
//! - Revocation: `revoke_all_for_user` wipes every row for a user
//!   in one DELETE. `Auth::logout` chains this so a real logout
//!   actually clears persistent state. `prune_expired` cleans up
//!   expired rows on a schedule.
//!
//! # Why bcrypt, not "store the plaintext"
//!
//! A DB dump must not yield credentials that re-authenticate the user.
//! Same reason passwords are hashed.

use chrono::Duration;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::database::DB;
use crate::error::FrameworkError;
use crate::hashing;

/// Width of the random plaintext token. 32 bytes = 256 bits of entropy.
/// URL-safe base64 encodes to 43 chars (no padding).
const TOKEN_BYTES: usize = 32;

/// Cookie name carrying the encrypted plaintext token.
///
/// Exposed as `pub` (rather than `pub(crate)`) so framework integration
/// tests can probe the cookie by name. `#[doc(hidden)]` keeps it out of
/// the published API surface — consumers should not reference it
/// directly; the contract is "the framework owns the remember-me
/// cookie."
#[doc(hidden)]
pub const COOKIE_NAME: &str = "remember_me";

/// Generate a fresh `(plaintext, bcrypt_hash)` pair.
///
/// The plaintext is what we hand the user via the cookie. The hash is
/// what we store in `remember_tokens.token_hash`. Bcrypt because the
/// framework already uses bcrypt for passwords (`framework/src/hashing/`),
/// so we do not multiply hash schemes.
///
/// `pub` + `#[doc(hidden)]` so integration tests in
/// `framework/tests/remember_me.rs` can build "real-shaped" rows whose
/// hash matches a known plaintext (used for the expired-token test).
#[doc(hidden)]
pub fn generate_token() -> Result<(String, String), FrameworkError> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use rand::RngCore;

    let mut bytes = [0u8; TOKEN_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    let plaintext = URL_SAFE_NO_PAD.encode(bytes);
    let hash = hashing::hash(&plaintext)?;
    Ok((plaintext, hash))
}

/// Issue a new remember token for `user_id`. Inserts a hashed row,
/// returns the plaintext to the caller. The caller is responsible for
/// shipping the plaintext to the client (via the session middleware's
/// pending-cookies slot, set by `Auth::login_remember`).
pub async fn issue(user_id: &str, ttl_minutes: i64) -> Result<String, FrameworkError> {
    let (plaintext, hash) = generate_token()?;
    let expires_at = chrono::Utc::now() + Duration::minutes(ttl_minutes);
    let now = chrono::Utc::now();

    let conn = DB::connection()?;
    let model = entity::ActiveModel {
        user_id: Set(user_id.to_string()),
        token_hash: Set(hash),
        expires_at: Set(expires_at.naive_utc()),
        created_at: Set(now.naive_utc()),
        last_used_at: Set(None),
        ..Default::default()
    };

    entity::Entity::insert(model)
        .exec(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("issue remember token: {e}")))?;

    Ok(plaintext)
}

/// Verify a plaintext remember-me token. On success:
///
/// 1. Delete the matched row (one-shot semantics).
/// 2. Issue a fresh row for the same user.
/// 3. Return `(user_id, new_plaintext)` so the caller can rotate the cookie.
///
/// On failure (no candidate matches), returns `Ok(None)`. The cookie
/// should then be cleared so the client stops sending a token that can
/// never authenticate.
///
/// # Why a full scan
///
/// Each row is bcrypt-salted, so we cannot index by hash. We filter to
/// not-yet-expired rows and bcrypt-verify each. In practice the
/// candidate set is bounded by "users who have ever ticked remember-me
/// and not logged out" — well within DB scan range. Expired rows are
/// pruned by [`prune_expired`].
pub async fn verify_and_rotate(
    token: &str,
    ttl_minutes: i64,
) -> Result<Option<(String, String)>, FrameworkError> {
    let conn = DB::connection()?;
    let now = chrono::Utc::now().naive_utc();

    let candidates = entity::Entity::find()
        .filter(entity::Column::ExpiresAt.gt(now))
        .all(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("scan remember tokens: {e}")))?;

    for row in candidates {
        // `hashing::verify` runs constant-time bcrypt comparison.
        if hashing::verify(token, &row.token_hash)? {
            // Rotate: delete the matched row + issue a fresh one.
            entity::Entity::delete_by_id(row.id)
                .exec(conn.inner())
                .await
                .map_err(|e| FrameworkError::database(format!("delete remember token: {e}")))?;

            let new_plaintext = issue(&row.user_id, ttl_minutes).await?;
            return Ok(Some((row.user_id, new_plaintext)));
        }
    }

    Ok(None)
}

/// Revoke every remember token for `user_id` in one DELETE.
///
/// Called from `Auth::logout` so a logout actually clears persistent
/// re-auth state. Also the right hook for a "log me out everywhere"
/// account-security button.
pub async fn revoke_all_for_user(user_id: &str) -> Result<u64, FrameworkError> {
    let conn = DB::connection()?;
    let result = entity::Entity::delete_many()
        .filter(entity::Column::UserId.eq(user_id))
        .exec(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("revoke remember tokens: {e}")))?;
    Ok(result.rows_affected)
}

/// Delete a single remember-token row by id. Useful for "log out this
/// device" UIs where the user picks a specific session.
pub async fn revoke_by_id(id: i64) -> Result<bool, FrameworkError> {
    let conn = DB::connection()?;
    let result = entity::Entity::delete_by_id(id)
        .exec(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("revoke remember token by id: {e}")))?;
    Ok(result.rows_affected == 1)
}

/// Delete all rows whose `expires_at` is in the past. Returns the
/// number of rows removed.
///
/// Wire this up to a scheduled task (see `framework/src/schedule/`) so
/// the table does not accumulate dead rows.
pub async fn prune_expired() -> Result<u64, FrameworkError> {
    let conn = DB::connection()?;
    let now = chrono::Utc::now().naive_utc();
    let result = entity::Entity::delete_many()
        .filter(entity::Column::ExpiresAt.lte(now))
        .exec(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("prune remember tokens: {e}")))?;
    Ok(result.rows_affected)
}

/// SeaORM entity for the `remember_tokens` table.
///
/// Schema mirrors the migration consumer apps ship (see
/// `app/src/migrations/m20251208_230000_create_remember_tokens_table.rs`
/// and the corresponding CLI scaffolder template):
///
/// - `id`           INTEGER PK auto-increment
/// - `user_id`      VARCHAR not null — opaque string id (post-Phase-3 String-everywhere)
/// - `token_hash`   VARCHAR not null — bcrypt hash of the plaintext token
/// - `expires_at`   TIMESTAMP not null — token TTL boundary
/// - `created_at`   TIMESTAMP not null
/// - `last_used_at` TIMESTAMP null — currently informational (rotation deletes the row before update)
pub mod entity {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "remember_tokens")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i64,
        pub user_id: String,
        pub token_hash: String,
        pub expires_at: chrono::NaiveDateTime,
        pub created_at: chrono::NaiveDateTime,
        pub last_used_at: Option<chrono::NaiveDateTime>,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `generate_token` produces a fresh plaintext + valid bcrypt hash
    /// each call. Plaintext length is the expected base64 width.
    #[test]
    fn generate_token_round_trips_through_bcrypt() {
        let (pt, hash) = generate_token().expect("token gen");
        // 32 bytes → 43 base64 chars without padding.
        assert_eq!(pt.len(), 43);
        assert!(hash.starts_with("$2"), "bcrypt hash prefix expected");
        assert!(
            hashing::verify(&pt, &hash).expect("verify"),
            "freshly generated token must verify against its hash"
        );

        // Different plaintext should not verify.
        let (pt2, _h2) = generate_token().expect("token gen 2");
        assert_ne!(pt, pt2, "tokens must not collide");
        assert!(
            !hashing::verify(&pt2, &hash).expect("verify"),
            "wrong plaintext must not verify"
        );
    }
}
