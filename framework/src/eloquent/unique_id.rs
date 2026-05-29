//! UUID / ULID primary-key generation — Laravel's `HasUuids` /
//! `HasUlids` / `HasVersion4Uuids` trait family.
//!
//! Suprnova exposes the same concept via the `#[model(unique_id = "...")]`
//! attribute. The macro recognises three values:
//!
//! - `"uuid"` (default UUID v7 — timestamp-ordered, matches Laravel 11+'s
//!   `Str::uuid7()` shape; the same flavour Laravel ships in `HasUuids`)
//! - `"uuid_v4"` (random UUID — matches Laravel's `HasVersion4Uuids`)
//! - `"ulid"` (lowercase ULID, 26 chars, matches Laravel's `HasUlids`)
//!
//! When set, the macro overrides the `Creating` lifecycle hook so that
//! before INSERT, the PK column receives a freshly-generated string ID
//! if the caller didn't supply one. The string ID lands in the same
//! cast pipeline as any other column — typing the PK as `String` on the
//! Rust struct is enough; the macro injects the generator into the
//! ActiveModel build path.
//!
//! ## Why no per-model trait
//!
//! Laravel's `HasUuids` is a trait that overrides several model
//! lifecycle hooks (`creating`, `getKeyType`, `getIncrementing`). In
//! Rust the equivalent surface is the macro attribute + the
//! [`UniqueIdGenerator`] trait below: the macro generates the
//! lifecycle wiring; the trait carries the runtime "what string do I
//! emit" choice.

use uuid::Uuid;

/// Generator strategy for a model's auto-populated string PK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniqueIdKind {
    /// UUID v7 — timestamp-ordered. Laravel-11+ default and the
    /// recommended choice for primary keys (index locality without
    /// sacrificing uniqueness). Encoded as a lowercase hyphenated
    /// 36-char string.
    UuidV7,
    /// UUID v4 — fully random. Mirrors Laravel's
    /// `HasVersion4Uuids` / `Str::orderedUuid` legacy path. Encoded as
    /// a lowercase hyphenated 36-char string.
    UuidV4,
    /// Lowercase ULID — 26 characters of Crockford base32 with a 48-bit
    /// timestamp prefix. Mirrors Laravel's `HasUlids`.
    Ulid,
}

impl UniqueIdKind {
    /// Parse a `unique_id = "..."` attribute value to a [`UniqueIdKind`].
    /// Accepts `"uuid"` (treated as v7 — matches Laravel 11+),
    /// `"uuid_v7"`, `"uuid_v4"`, and `"ulid"`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "uuid" | "uuid_v7" => Some(Self::UuidV7),
            "uuid_v4" => Some(Self::UuidV4),
            "ulid" => Some(Self::Ulid),
            _ => None,
        }
    }

    /// Whether the generated ID is valid for this kind. Mirrors
    /// Laravel's `isValidUniqueId` check. Used by `find` to refuse
    /// malformed identifiers before they reach the database.
    pub fn is_valid(&self, value: &str) -> bool {
        match self {
            Self::UuidV7 | Self::UuidV4 => Uuid::parse_str(value).is_ok(),
            Self::Ulid => is_valid_ulid(value),
        }
    }

    /// Emit a fresh ID of this kind.
    pub fn generate(&self) -> String {
        match self {
            Self::UuidV7 => Uuid::now_v7().to_string(),
            Self::UuidV4 => Uuid::new_v4().to_string(),
            Self::Ulid => generate_ulid_lowercase(),
        }
    }
}

/// Sealed trait the macro emits on every `#[model(unique_id = "...")]`
/// struct. Carries the per-model kind so framework code (and user
/// code) can introspect the strategy without parsing the attribute
/// string again.
pub trait HasUniqueId {
    /// The generator strategy for this model.
    const UNIQUE_ID_KIND: UniqueIdKind;

    /// Generate a fresh unique ID for this model. Defaults to
    /// [`UniqueIdKind::generate`] for [`Self::UNIQUE_ID_KIND`].
    /// Override to swap in a custom generator (e.g. a hash with a
    /// project-specific prefix).
    fn new_unique_id() -> String {
        Self::UNIQUE_ID_KIND.generate()
    }
}

/// Crockford base32 alphabet for ULID encoding. Matches the
/// `ulid-go` / `ulid.js` / Laravel `Str::ulid()` outputs.
const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Generate a 26-character lowercase ULID. We pull a 48-bit Unix-ms
/// timestamp + 80 bits of randomness, encode the 128 bits in Crockford
/// base32, then lowercase. Mirrors Laravel's `Str::ulid()` then
/// `strtolower(...)` path in `HasUlids::newUniqueId`.
fn generate_ulid_lowercase() -> String {
    use rand::RngExt;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut rng = rand::rng();
    let mut rand_bytes = [0u8; 10];
    for b in &mut rand_bytes {
        *b = rng.random_range(0u32..256u32) as u8;
    }

    let mut buf = [0u8; 16];
    // High 6 bytes of the timestamp into the first 6 bytes of buf.
    // ULID stores only 48 bits; mask to that width.
    let ts48 = now_ms & ((1u64 << 48) - 1);
    buf[0] = ((ts48 >> 40) & 0xff) as u8;
    buf[1] = ((ts48 >> 32) & 0xff) as u8;
    buf[2] = ((ts48 >> 24) & 0xff) as u8;
    buf[3] = ((ts48 >> 16) & 0xff) as u8;
    buf[4] = ((ts48 >> 8) & 0xff) as u8;
    buf[5] = (ts48 & 0xff) as u8;
    buf[6..].copy_from_slice(&rand_bytes);

    // 128 bits → 26 Crockford base32 chars. ULID encodes the first
    // char from only the top 2 bits to keep the canonical length.
    let mut out = String::with_capacity(26);
    out.push(CROCKFORD[((buf[0] & 0xE0) >> 5) as usize] as char);
    out.push(CROCKFORD[(buf[0] & 0x1F) as usize] as char);
    out.push(CROCKFORD[((buf[1] & 0xF8) >> 3) as usize] as char);
    out.push(CROCKFORD[(((buf[1] & 0x07) << 2) | ((buf[2] & 0xC0) >> 6)) as usize] as char);
    out.push(CROCKFORD[((buf[2] & 0x3E) >> 1) as usize] as char);
    out.push(CROCKFORD[(((buf[2] & 0x01) << 4) | ((buf[3] & 0xF0) >> 4)) as usize] as char);
    out.push(CROCKFORD[(((buf[3] & 0x0F) << 1) | ((buf[4] & 0x80) >> 7)) as usize] as char);
    out.push(CROCKFORD[((buf[4] & 0x7C) >> 2) as usize] as char);
    out.push(CROCKFORD[(((buf[4] & 0x03) << 3) | ((buf[5] & 0xE0) >> 5)) as usize] as char);
    out.push(CROCKFORD[(buf[5] & 0x1F) as usize] as char);
    // Randomness section — 16 chars from the 10 random bytes.
    out.push(CROCKFORD[((buf[6] & 0xF8) >> 3) as usize] as char);
    out.push(CROCKFORD[(((buf[6] & 0x07) << 2) | ((buf[7] & 0xC0) >> 6)) as usize] as char);
    out.push(CROCKFORD[((buf[7] & 0x3E) >> 1) as usize] as char);
    out.push(CROCKFORD[(((buf[7] & 0x01) << 4) | ((buf[8] & 0xF0) >> 4)) as usize] as char);
    out.push(CROCKFORD[(((buf[8] & 0x0F) << 1) | ((buf[9] & 0x80) >> 7)) as usize] as char);
    out.push(CROCKFORD[((buf[9] & 0x7C) >> 2) as usize] as char);
    out.push(CROCKFORD[(((buf[9] & 0x03) << 3) | ((buf[10] & 0xE0) >> 5)) as usize] as char);
    out.push(CROCKFORD[(buf[10] & 0x1F) as usize] as char);
    out.push(CROCKFORD[((buf[11] & 0xF8) >> 3) as usize] as char);
    out.push(CROCKFORD[(((buf[11] & 0x07) << 2) | ((buf[12] & 0xC0) >> 6)) as usize] as char);
    out.push(CROCKFORD[((buf[12] & 0x3E) >> 1) as usize] as char);
    out.push(CROCKFORD[(((buf[12] & 0x01) << 4) | ((buf[13] & 0xF0) >> 4)) as usize] as char);
    out.push(CROCKFORD[(((buf[13] & 0x0F) << 1) | ((buf[14] & 0x80) >> 7)) as usize] as char);
    out.push(CROCKFORD[((buf[14] & 0x7C) >> 2) as usize] as char);
    out.push(CROCKFORD[(((buf[14] & 0x03) << 3) | ((buf[15] & 0xE0) >> 5)) as usize] as char);
    out.push(CROCKFORD[(buf[15] & 0x1F) as usize] as char);

    out.to_ascii_lowercase()
}

/// Is `value` a valid lowercase Crockford-base32 ULID? Length must be
/// exactly 26 and every character must appear in [`CROCKFORD`]
/// (case-insensitive — Laravel emits lowercase but lib readers may
/// accept either).
fn is_valid_ulid(value: &str) -> bool {
    if value.len() != 26 {
        return false;
    }
    value
        .bytes()
        .all(|b| CROCKFORD.contains(&b.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulid_round_trip_length_and_alphabet() {
        for _ in 0..100 {
            let id = UniqueIdKind::Ulid.generate();
            assert_eq!(id.len(), 26);
            assert!(UniqueIdKind::Ulid.is_valid(&id), "invalid ulid: {id}");
        }
    }

    #[test]
    fn uuid_v7_round_trip() {
        for _ in 0..100 {
            let id = UniqueIdKind::UuidV7.generate();
            assert_eq!(id.len(), 36);
            assert!(UniqueIdKind::UuidV7.is_valid(&id));
        }
    }

    #[test]
    fn uuid_v4_round_trip() {
        let id = UniqueIdKind::UuidV4.generate();
        assert!(UniqueIdKind::UuidV4.is_valid(&id));
    }

    #[test]
    fn parse_attributes() {
        assert_eq!(UniqueIdKind::parse("uuid"), Some(UniqueIdKind::UuidV7));
        assert_eq!(UniqueIdKind::parse("uuid_v7"), Some(UniqueIdKind::UuidV7));
        assert_eq!(UniqueIdKind::parse("uuid_v4"), Some(UniqueIdKind::UuidV4));
        assert_eq!(UniqueIdKind::parse("ulid"), Some(UniqueIdKind::Ulid));
        assert_eq!(UniqueIdKind::parse("nope"), None);
    }

    #[test]
    fn reject_invalid_strings() {
        assert!(!UniqueIdKind::Ulid.is_valid("too-short"));
        assert!(!UniqueIdKind::UuidV7.is_valid("not a uuid"));
    }
}
