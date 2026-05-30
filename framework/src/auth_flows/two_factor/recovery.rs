//! Single-use TOTP recovery codes.
//!
//! Persisted as newline-joined plaintext inside
//! `two_factor_credentials.recovery_codes`, then encrypted at rest via
//! [`crate::crypto::Crypt`]. Consuming a code rewrites the column with
//! the remaining codes (or sets it to `NULL` once the list is empty),
//! making consumption single-use and atomic per row update.

use crate::auth_flows::two_factor::entity;
use crate::crypto::Crypt;
use crate::database::DB;
use crate::error::FrameworkError;
use rand::RngExt;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, EntityTrait};

/// Generate `count` fresh recovery codes in `NNNNNN-NNNNNN` shape
/// (12 decimal digits with a hyphen separator, ~40 bits of entropy
/// each — matching Laravel's `Fortify::recoveryCodes()` format).
pub fn generate(count: usize) -> Vec<String> {
    let mut rng = rand::rng();
    (0..count)
        .map(|_| {
            let a: u32 = rng.random_range(0..1_000_000);
            let b: u32 = rng.random_range(0..1_000_000);
            format!("{a:06}-{b:06}")
        })
        .collect()
}

/// Attempt to consume one recovery code for `user_id`. Returns
/// `Ok(true)` if the code matched and was removed; `Ok(false)` if no
/// active row exists, no recovery codes are stored, or the supplied
/// code does not appear in the list. Storage failures and decryption
/// failures surface as `Err`.
pub async fn consume(user_id: &str, code: &str) -> Result<bool, FrameworkError> {
    let db = DB::connection()?;
    let conn = db.inner();
    let Some(row) = entity::Entity::find_by_id(user_id.to_string())
        .one(conn)
        .await
        .map_err(|e| FrameworkError::internal(format!("two_factor find: {e}")))?
    else {
        return Ok(false);
    };
    let Some(encrypted) = row.recovery_codes.clone() else {
        return Ok(false);
    };
    let plaintext =
        Crypt::decrypt_string(crate::crypto::CryptPurpose::TwoFactorRecovery, &encrypted)?;
    let mut codes: Vec<String> = plaintext.lines().map(String::from).collect();
    let Some(idx) = codes.iter().position(|c| c == code) else {
        return Ok(false);
    };
    codes.remove(idx);
    let new_recovery = if codes.is_empty() {
        None
    } else {
        Some(Crypt::encrypt_string(
            crate::crypto::CryptPurpose::TwoFactorRecovery,
            &codes.join("\n"),
        )?)
    };
    let mut active: entity::ActiveModel = row.into();
    active.recovery_codes = Set(new_recovery);
    active.updated_at = Set(chrono::Utc::now());
    active
        .update(conn)
        .await
        .map_err(|e| FrameworkError::internal(format!("two_factor update: {e}")))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::generate;

    #[test]
    fn generate_returns_requested_count_of_distinct_codes() {
        let codes = generate(10);
        assert_eq!(codes.len(), 10);
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), 10, "codes collided: {codes:?}");
    }

    #[test]
    fn generate_codes_match_expected_shape() {
        for c in generate(20) {
            assert_eq!(c.len(), 13, "{c}");
            let (a, b) = c.split_once('-').expect("hyphen separator");
            assert_eq!(a.len(), 6);
            assert_eq!(b.len(), 6);
            assert!(a.chars().all(|c| c.is_ascii_digit()));
            assert!(b.chars().all(|c| c.is_ascii_digit()));
        }
    }
}
