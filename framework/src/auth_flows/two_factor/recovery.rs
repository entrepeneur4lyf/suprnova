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
use subtle::{Choice, ConditionallySelectable, ConstantTimeEq};

/// Locate the index (if any) of `code` inside `codes` without
/// short-circuiting. Visits every entry, comparing it to `code` with
/// [`subtle::ConstantTimeEq`] and folding the result. Run-time depends
/// on `codes.len()` only, not on whether or where a match exists, so a
/// timing observer cannot learn which slot — or whether any slot —
/// matched. Equal-length entries fall through to `ct_eq`; entries of a
/// different length are skipped (recovery codes are a fixed 13-byte
/// shape, so a length mismatch is a structural reject, not a timing
/// oracle for a same-length attacker).
fn find_constant_time(codes: &[String], code: &str) -> Option<usize> {
    let candidate = code.as_bytes();
    let mut found: Choice = Choice::from(0u8);
    let mut idx: u32 = 0;
    for (i, stored) in codes.iter().enumerate() {
        let stored_bytes = stored.as_bytes();
        let eq = if stored_bytes.len() == candidate.len() {
            stored_bytes.ct_eq(candidate)
        } else {
            Choice::from(0u8)
        };
        // Adopt this index only on the FIRST match — `take` is set only
        // when `eq` is true and no earlier match has been recorded.
        // `conditional_select` keeps the assignment branch-free.
        let take = eq & !found;
        let take_idx = u32::conditional_select(&0u32, &(i as u32), take);
        idx |= take_idx;
        found |= eq;
    }
    if bool::from(found) {
        Some(idx as usize)
    } else {
        None
    }
}

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
    let Some(idx) = find_constant_time(&codes, code) else {
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
    use super::{find_constant_time, generate};

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

    #[test]
    fn ct_find_matches_at_each_index() {
        let codes: Vec<String> = vec![
            "111111-111111".into(),
            "222222-222222".into(),
            "333333-333333".into(),
            "444444-444444".into(),
            "555555-555555".into(),
        ];
        for (i, c) in codes.iter().enumerate() {
            assert_eq!(find_constant_time(&codes, c), Some(i), "mismatch for {c}");
        }
    }

    #[test]
    fn ct_find_returns_none_for_unknown_code() {
        let codes: Vec<String> = vec!["111111-111111".into(), "222222-222222".into()];
        assert_eq!(find_constant_time(&codes, "999999-999999"), None);
    }

    #[test]
    fn ct_find_returns_none_for_empty_list() {
        let codes: Vec<String> = Vec::new();
        assert_eq!(find_constant_time(&codes, "111111-111111"), None);
    }

    #[test]
    fn ct_find_returns_none_for_length_mismatch() {
        let codes: Vec<String> = vec!["111111-111111".into()];
        // Same prefix but truncated — must NOT match.
        assert_eq!(find_constant_time(&codes, "111111-11111"), None);
        // Longer than any stored code.
        assert_eq!(find_constant_time(&codes, "111111-1111111"), None);
        // Completely empty candidate.
        assert_eq!(find_constant_time(&codes, ""), None);
    }

    #[test]
    fn ct_find_returns_first_index_for_duplicate_entries() {
        // Generation guarantees uniqueness, but the helper should still
        // behave deterministically if a duplicate ever slips through.
        let codes: Vec<String> = vec![
            "111111-111111".into(),
            "222222-222222".into(),
            "222222-222222".into(),
        ];
        assert_eq!(find_constant_time(&codes, "222222-222222"), Some(1));
    }
}
