//! Remember-me tokens (codex review finding #13; selector+verifier
//! hardening from ChatGPT audit findings `auth` HIGH #1 + #2).
//!
//! "Remember me" is the cookie that re-authenticates a user after their
//! session expires or their browser is closed. Done wrong it is a long-
//! lived bearer credential sitting in plaintext on a user's disk and in
//! a database backup — done right it is a rotating, single-use,
//! short-fuse re-auth handle with constant-cost verification.
//!
//! # Design — selector + verifier scheme
//!
//! Each issued cookie is a two-part composite plaintext token
//! `"{selector}.{verifier}"` where:
//!
//! - **selector** — 16 random bytes (128 bits), URL-safe base64
//!   (22 chars, no padding). Stored plaintext on a UNIQUE indexed
//!   column. The selector is the lookup key — one indexed query
//!   identifies the exactly-one candidate row to verify, so
//!   verification cost is O(1) regardless of how many active tokens
//!   the table holds.
//!
//! - **verifier** — 32 random bytes (256 bits), URL-safe base64
//!   (43 chars, no padding). Bcrypt-hashed before storage. The
//!   plaintext verifier lives only inside the encrypted `remember_me`
//!   cookie. Verification is exactly one constant-time
//!   `bcrypt::verify` against the selector-matched row.
//!
//! # Why both parts
//!
//! A pure hash-only design (no selector) had two problems documented by
//! audit findings:
//!
//! 1. **Unbounded bcrypt scan on forged cookies.** Each row's bcrypt
//!    hash is per-row salted, so without a selector we had to scan
//!    every unexpired row and bcrypt-verify each — O(N) bcrypt work per
//!    request, attacker-controlled.
//!
//! 2. **Non-single-use rotation under concurrency.** Two concurrent
//!    requests could both load the same row, both pass `bcrypt::verify`,
//!    and one of the DELETEs would affect zero rows — but both still
//!    minted replacement tokens. The selector-keyed atomic-DELETE
//!    pattern (`DELETE ... WHERE id = ? AND selector = ?`, then check
//!    `rows_affected`) makes rotation single-use even under concurrency.
//!
//! A pure-selector design (no verifier hashed) would mean a DB dump
//! yields re-authenticating credentials — same standard as why
//! passwords are hashed. The verifier preserves the "DB dump is not
//! enough" property.
//!
//! # Lifecycle
//!
//! - **Issuance**: random selector + random verifier → bcrypt-hash
//!   the verifier → INSERT row → encrypt `"{selector}.{verifier}"`
//!   under `Crypt` → place the encrypted blob in the `remember_me`
//!   cookie.
//!
//! - **Verification**: decrypt cookie → split into
//!   `(selector, verifier)` → `SELECT row WHERE selector = ? AND
//!   expires_at > now LIMIT 1` → `bcrypt::verify(verifier,
//!   row.token_hash)`. On match, atomic conditional DELETE keyed on
//!   `(id, selector)`; the loser of a race sees `rows_affected == 0`
//!   and returns `None` (replay defeated).
//!
//! - **Rotation**: a successful verify DELETEs the matched row and
//!   issues a fresh selector+verifier pair. The middleware re-sets the
//!   cookie with the new composite plaintext.
//!
//! - **Revocation**: [`revoke_all_for_user`] wipes every row for a user
//!   in one DELETE. `Auth::logout` chains this so a real logout
//!   actually clears persistent state. [`prune_expired`] cleans up
//!   expired rows on a schedule.
//!
//! # Cookie format
//!
//! The on-wire cookie value is `Crypt::encrypt_string("{selector}.{verifier}")`.
//! A successful decrypt yields the composite plaintext; the framework
//! never stores or returns either half on its own. Tokens whose
//! plaintext lacks the `.` separator are silently rejected (no DB hit,
//! no bcrypt cost).
//!
//! # Why bcrypt, not "store the verifier plaintext"
//!
//! A DB dump must not yield credentials that re-authenticate the user.
//! Same reason passwords are hashed.

use chrono::Duration;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};

use crate::database::DB;
use crate::error::FrameworkError;
use crate::hashing;

/// Width of the selector. 16 bytes = 128 bits — enough collision
/// resistance for the lookup key over the lifetime of the deployment.
/// URL-safe base64 encodes to 22 chars (no padding).
const SELECTOR_BYTES: usize = 16;

/// Width of the verifier plaintext. 32 bytes = 256 bits of entropy.
/// URL-safe base64 encodes to 43 chars (no padding).
const VERIFIER_BYTES: usize = 32;

/// Cookie name carrying the encrypted plaintext token.
///
/// Exposed as `pub` (rather than `pub(crate)`) so framework integration
/// tests can probe the cookie by name. `#[doc(hidden)]` keeps it out of
/// the published API surface — consumers should not reference it
/// directly; the contract is "the framework owns the remember-me
/// cookie."
#[doc(hidden)]
pub const COOKIE_NAME: &str = "remember_me";

/// Generate a fresh `(selector, verifier_plaintext, verifier_hash)`
/// triple.
///
/// The composite `"{selector}.{verifier_plaintext}"` is what we hand
/// the user via the cookie. The selector is stored plaintext (indexed,
/// for O(1) lookup) and the hash is what we store in
/// `remember_tokens.token_hash`. Bcrypt because the framework already
/// uses bcrypt for passwords (`framework/src/hashing/`), so we do not
/// multiply hash schemes.
///
/// `pub` + `#[doc(hidden)]` so integration tests in
/// `framework/tests/remember_me.rs` can build "real-shaped" rows whose
/// hash matches a known verifier (used for the expired-token test).
#[doc(hidden)]
pub async fn generate_token() -> Result<(String, String, String), FrameworkError> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    // High-entropy OS randomness for both halves. `getrandom::fill`
    // is the direct binding to the OS RNG (e.g. `getrandom(2)` on
    // Linux, `BCryptGenRandom` on Windows). rand 0.10 removed `OsRng`
    // from its public surface, so call getrandom directly.
    let mut selector_bytes = [0u8; SELECTOR_BYTES];
    getrandom::fill(&mut selector_bytes)
        .map_err(|e| FrameworkError::internal(format!("OS RNG failure (selector): {e}")))?;
    let selector = URL_SAFE_NO_PAD.encode(selector_bytes);

    let mut verifier_bytes = [0u8; VERIFIER_BYTES];
    getrandom::fill(&mut verifier_bytes)
        .map_err(|e| FrameworkError::internal(format!("OS RNG failure (verifier): {e}")))?;
    let verifier_plaintext = URL_SAFE_NO_PAD.encode(verifier_bytes);

    // Audit HIGH `hashing` #1 — bcrypt is CPU-bound (~250ms @ cost 12).
    // Use the async variant so the Tokio worker thread isn't blocked
    // while the framework issues a token under request load.
    let verifier_hash = hashing::hash_async(&verifier_plaintext).await?;
    Ok((selector, verifier_plaintext, verifier_hash))
}

/// Issue a new remember token for `user_id`. Inserts a row keyed on a
/// random selector and storing the bcrypt hash of the verifier;
/// returns the composite plaintext `"{selector}.{verifier}"` to the
/// caller. The caller is responsible for shipping the plaintext to the
/// client (via the session middleware's pending-cookies slot, set by
/// `Auth::login_remember`).
pub async fn issue(user_id: &str, ttl_minutes: i64) -> Result<String, FrameworkError> {
    let (selector, verifier_plaintext, verifier_hash) = generate_token().await?;
    let expires_at = chrono::Utc::now() + Duration::minutes(ttl_minutes);
    let now = chrono::Utc::now();

    let conn = DB::connection()?;
    let model = entity::ActiveModel {
        user_id: Set(user_id.to_string()),
        selector: Set(selector.clone()),
        token_hash: Set(verifier_hash),
        expires_at: Set(expires_at.naive_utc()),
        created_at: Set(now.naive_utc()),
        last_used_at: Set(None),
        ..Default::default()
    };

    entity::Entity::insert(model)
        .exec(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("issue remember token: {e}")))?;

    Ok(format!("{selector}.{verifier_plaintext}"))
}

/// Verify a plaintext remember-me token. On success:
///
/// 1. Atomic conditional DELETE of the matched row keyed on
///    `(id, selector)`. Race-loser sees `rows_affected == 0` and bails.
/// 2. Issue a fresh row for the same user.
/// 3. Return `(user_id, new_composite_plaintext)` so the caller can
///    rotate the cookie.
///
/// On failure (malformed token, no candidate matches, verifier
/// mismatch, or rotation race lost), returns `Ok(None)`. The cookie
/// should then be cleared so the client stops sending a token that can
/// never authenticate.
///
/// # Verification cost
///
/// Exactly one indexed SELECT + at most one `bcrypt::verify` per
/// request — independent of how many active tokens the table holds.
/// Forged cookies cost only the SELECT (the verifier never matches
/// because the selector did not, or `expires_at` already passed).
pub async fn verify_and_rotate(
    token: &str,
    ttl_minutes: i64,
) -> Result<Option<(String, String)>, FrameworkError> {
    // Parse the composite token. Malformed → no auth (and no DB hit).
    let (selector, verifier) = match token.split_once('.') {
        Some(pair) => pair,
        None => return Ok(None),
    };

    let conn = DB::connection()?;
    let now = chrono::Utc::now().naive_utc();

    // O(1) indexed lookup: the UNIQUE constraint on `selector` means
    // this returns 0 or 1 rows.
    let row = entity::Entity::find()
        .filter(entity::Column::Selector.eq(selector))
        .filter(entity::Column::ExpiresAt.gt(now))
        .one(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("look up remember token: {e}")))?;

    let row = match row {
        Some(r) => r,
        None => return Ok(None),
    };

    // Exactly one `bcrypt::verify` per request — constant-time
    // comparison, no scanning. Audit HIGH `hashing` #1: the async
    // variant runs the CPU-bound bcrypt verification on
    // `spawn_blocking` so the request worker thread stays free.
    if !hashing::verify_async(verifier, &row.token_hash).await? {
        return Ok(None);
    }

    // Atomic conditional DELETE — succeeds for exactly one concurrent
    // verifier of this token. If two requests both reach this point,
    // exactly one DELETE affects 1 row and the other affects 0. The
    // loser MUST NOT issue a fresh token: that would defeat the
    // single-use rotation invariant by minting two replacements for
    // the same captured cookie.
    let delete_result = entity::Entity::delete_many()
        .filter(entity::Column::Id.eq(row.id))
        .filter(entity::Column::Selector.eq(&row.selector))
        .exec(conn.inner())
        .await
        .map_err(|e| FrameworkError::database(format!("delete remember token: {e}")))?;

    if delete_result.rows_affected != 1 {
        // Lost the rotation race. Another concurrent request already
        // deleted this row and minted a fresh one. Treat this attempt
        // as "no auth" — replay defeated.
        return Ok(None);
    }

    let new_composite = issue(&row.user_id, ttl_minutes).await?;
    Ok(Some((row.user_id, new_composite)))
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
/// - `selector`     VARCHAR not null UNIQUE — 22-char URL-safe base64 lookup key
/// - `token_hash`   VARCHAR not null — bcrypt hash of the verifier plaintext
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
        #[sea_orm(unique)]
        pub selector: String,
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

    /// `generate_token` produces a fresh selector + verifier + matching
    /// hash on each call.
    #[tokio::test]
    async fn generate_token_round_trips_through_bcrypt() {
        let (sel, ver, hash) = generate_token().await.expect("token gen");
        // 16 bytes → 22 base64 chars, 32 bytes → 43 base64 chars.
        assert_eq!(sel.len(), 22, "selector is 22 base64 chars (16 bytes)");
        assert_eq!(ver.len(), 43, "verifier is 43 base64 chars (32 bytes)");
        assert!(hash.starts_with("$2"), "bcrypt hash prefix expected");
        assert!(
            hashing::verify(&ver, &hash).expect("verify"),
            "freshly generated verifier must verify against its hash"
        );

        // Different generations must produce different selectors AND
        // different verifiers.
        let (sel2, ver2, _h2) = generate_token().await.expect("token gen 2");
        assert_ne!(sel, sel2, "selectors must not collide");
        assert_ne!(ver, ver2, "verifiers must not collide");
        assert!(
            !hashing::verify(&ver2, &hash).expect("verify"),
            "wrong verifier must not verify"
        );
    }
}
