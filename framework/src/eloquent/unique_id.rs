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
/// timestamp + 80 bits of randomness, pack them into the canonical
/// 16-byte ULID layout, then encode with [`encode_ulid_lowercase`].
/// Mirrors Laravel's `Str::ulid()` then `strtolower(...)` path in
/// `HasUlids::newUniqueId`.
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

    encode_ulid_lowercase(&buf)
}

/// Encode a packed 16-byte ULID payload (6-byte timestamp + 10-byte
/// random) into the canonical 26-character Crockford base32 string,
/// lowercased.
///
/// Pulled out of [`generate_ulid_lowercase`] so the bit-packing logic
/// is deterministic and testable against known vectors — the
/// timestamp + random source in `generate_ulid_lowercase` is not.
///
/// ULID encodes 128 bits into 26 base32 characters; the leading char
/// carries only the top 2 bits of `buf[0]` (the remaining 3 bits would
/// overflow a 130-bit space). Matches the canonical ULID spec and
/// every mainstream implementation (`ulid-go`, `ulid.js`,
/// Laravel `Str::ulid()`).
fn encode_ulid_lowercase(buf: &[u8; 16]) -> String {
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

    /// All-zero payload encodes to 26 `'0'` characters — the canonical
    /// minimum ULID. Verifies the bit-packing handles the zero edge
    /// case across every 5-bit slice without offset bugs.
    #[test]
    fn encode_zero_payload_matches_spec_minimum() {
        let buf = [0u8; 16];
        assert_eq!(encode_ulid_lowercase(&buf), "00000000000000000000000000");
    }

    /// All-ones payload encodes to `"7zzzzzzzzzzzzzzzzzzzzzzzzz"` — the
    /// canonical maximum ULID per the spec. The leading char is `'7'`
    /// (not `'Z'`) because the first character carries only the top 2
    /// bits of `buf[0]`; the remaining 25 chars each span a full 5-bit
    /// slice of all-ones, which is Crockford index 31 = `'Z'`.
    #[test]
    fn encode_max_payload_matches_spec_maximum() {
        let buf = [0xffu8; 16];
        assert_eq!(encode_ulid_lowercase(&buf), "7zzzzzzzzzzzzzzzzzzzzzzzzz");
    }

    /// A known asymmetric vector: timestamp = 1 ms, randomness = 0.
    /// The smallest non-zero ts48 puts a `1` in `buf[5]` only.
    /// Char 10 reads `buf[5] & 0x1F` = 1 → Crockford index 1 = `'1'`;
    /// every other char reads zero bits and stays `'0'`. Catches any
    /// off-by-one in the per-position bit masks.
    #[test]
    fn encode_minimum_nonzero_timestamp_isolated_in_position_10() {
        let mut buf = [0u8; 16];
        buf[5] = 0x01;
        assert_eq!(encode_ulid_lowercase(&buf), "00000000010000000000000000");
    }

    /// Decode a Crockford base32 ULID string back to the underlying 16
    /// bytes using an INDEPENDENT bit-shift algorithm (accumulator-based
    /// rather than position-by-position), then verify the original
    /// payload survives a round-trip. Catches any deviation between
    /// `encode_ulid_lowercase` and a textbook base32 reader.
    #[test]
    fn random_payloads_round_trip_through_independent_decoder() {
        // Vector A: timestamp + low-bit random, exercising every
        // 5-bit slice at least partially.
        let payloads: [[u8; 16]; 4] = [
            [
                0x01, 0x86, 0xe5, 0xf2, 0xd1, 0x40, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0,
                0x11, 0x22,
            ],
            [
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
                0xff, 0xff,
            ],
            [
                0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ],
            [
                0xde, 0xad, 0xbe, 0xef, 0xfe, 0xed, 0xfa, 0xce, 0xca, 0xfe, 0xba, 0xbe, 0xc0, 0xff,
                0xee, 0x42,
            ],
        ];
        for buf in &payloads {
            let encoded = encode_ulid_lowercase(buf);
            assert_eq!(encoded.len(), 26, "ulid must be 26 chars: {encoded}");
            let decoded = decode_crockford_ulid(&encoded)
                .unwrap_or_else(|| panic!("decode failed for {encoded}"));
            assert_eq!(
                &decoded, buf,
                "round-trip mismatch — input {buf:02x?}, encoded {encoded}, decoded {decoded:02x?}",
            );
        }
    }

    /// Independent reference decoder for round-trip verification.
    /// Pulls 5 bits at a time off the front of an accumulator. The
    /// leading character contributes 3 bits, every subsequent
    /// character contributes 5 bits — 3 + 25*5 = 128 bits, the exact
    /// ULID width.
    ///
    /// A well-formed ULID's leading char satisfies `index <= 7`
    /// (3 bits). Higher values would imply a 129-bit payload; we
    /// reject those as non-canonical input rather than truncate.
    ///
    /// Returns `None` on length, alphabet, or leading-char overflow.
    fn decode_crockford_ulid(s: &str) -> Option<[u8; 16]> {
        if s.len() != 26 {
            return None;
        }
        let chars: Vec<u8> = s.bytes().collect();
        let mut indices = [0u8; 26];
        for (i, c) in chars.iter().enumerate() {
            let up = c.to_ascii_uppercase();
            let idx = CROCKFORD.iter().position(|&x| x == up)?;
            indices[i] = idx as u8;
        }
        if indices[0] > 0b111 {
            return None;
        }
        let mut acc: u128 = indices[0] as u128 & 0b111;
        for &i in &indices[1..] {
            acc = (acc << 5) | (i as u128 & 0x1F);
        }
        Some(acc.to_be_bytes())
    }

    /// `generate_ulid_lowercase` embeds the current Unix timestamp ms
    /// in the first 6 bytes (10 base32 chars). Decode the first 10
    /// chars of a freshly-generated ULID and verify the timestamp
    /// falls within ±2 seconds of `SystemTime::now()`. This exercises
    /// the same encoding path as the encoder unit tests while binding
    /// the timestamp source to real wall-clock semantics.
    #[test]
    fn generated_ulid_timestamp_decodes_close_to_now() {
        let captured_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let id = generate_ulid_lowercase();
        let bytes = decode_crockford_ulid(&id)
            .unwrap_or_else(|| panic!("generated ULID must decode: {id}"));
        // First 6 bytes = 48-bit big-endian Unix ms timestamp.
        let mut ts_be = [0u8; 8];
        ts_be[2..].copy_from_slice(&bytes[..6]);
        let ts = u64::from_be_bytes(ts_be);
        // 2 seconds of tolerance — plenty of slack for any CI scheduler hiccup.
        const TOLERANCE_MS: u64 = 2_000;
        let diff = ts.abs_diff(captured_ms);
        assert!(
            diff <= TOLERANCE_MS,
            "ULID timestamp {ts} drifts {diff} ms from now {captured_ms} (id: {id})",
        );
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
