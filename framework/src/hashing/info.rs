//! Hash-string inspection — `info()` + `is_hashed()` + recognised-algo
//! parsing.
//!
//! Equivalent to Laravel's `password_get_info()`. The parser recognises:
//!
//! - **bcrypt MCF** — `$2a$<cost>$<salt-22><digest-31>` (60 chars total),
//!   also `$2b$`, `$2x$`, `$2y$` variants; `2b` is the current canonical
//!   form, `2a`/`2x`/`2y` are legacy variants that should be rehashed.
//! - **Argon2 PHC** — `$argon2{i,id,d}$v=<ver>$m=<kib>,t=<iters>,p=<lanes>$<salt-b64>$<digest-b64>`.
//!   Parsed via the `password-hash`-aligned helper that `argon2` crate
//!   re-exports as `argon2::PasswordHash`.
//!
//! Anything else returns `HashInfo { algo: Unknown, .. }` so callers can
//! treat it as "needs rehash" without crashing on malformed input.

use super::config::Algorithm;

/// Parsed hash metadata.
#[derive(Debug, Clone)]
pub struct HashInfo {
    /// Recognised algorithm, or [`Algorithm::Bcrypt`]-shaped variant /
    /// unknown if the hash matches no recognised prefix.
    pub algo: AlgoName,
    /// Bcrypt cost / argon time iterations. `None` for unknown.
    pub rounds: Option<u32>,
    /// Argon memory cost in KiB. `None` for bcrypt or unknown.
    pub memory: Option<u32>,
    /// Argon time iterations. `None` for bcrypt or unknown.
    pub time: Option<u32>,
    /// Argon parallelism / lanes. `None` for bcrypt or unknown.
    pub threads: Option<u32>,
    /// Raw bcrypt variant prefix when [`AlgoName::Bcrypt`]: `2a`, `2b`,
    /// `2x`, `2y`. `None` for argon / unknown.
    pub bcrypt_variant: Option<&'static str>,
}

/// Algorithm name surfaced by [`HashInfo`]. Distinct from
/// [`super::config::Algorithm`] because it can be `Unknown` (the config
/// type only spans the *supported* algorithms).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlgoName {
    /// Any `$2*$`-prefixed hash. The legacy/canonical distinction is in
    /// [`HashInfo::bcrypt_variant`].
    Bcrypt,
    /// `$argon2i$…`
    Argon2i,
    /// `$argon2id$…`
    Argon2id,
    /// `$argon2d$…` — accepted by parser, but Suprnova doesn't mint it
    /// (Laravel doesn't either; argon2d is GPU-target-only).
    Argon2d,
    /// Anything the parser doesn't recognise. Always rehash on next use.
    Unknown,
}

impl AlgoName {
    /// Map to the supported [`Algorithm`] enum, dropping `Argon2d` and
    /// `Unknown`.
    pub fn supported(&self) -> Option<Algorithm> {
        match self {
            AlgoName::Bcrypt => Some(Algorithm::Bcrypt),
            AlgoName::Argon2i => Some(Algorithm::Argon2i),
            AlgoName::Argon2id => Some(Algorithm::Argon2id),
            AlgoName::Argon2d | AlgoName::Unknown => None,
        }
    }

    /// String label matching Laravel's `algoName` values.
    pub fn as_str(&self) -> &'static str {
        match self {
            AlgoName::Bcrypt => "bcrypt",
            AlgoName::Argon2i => "argon2i",
            AlgoName::Argon2id => "argon2id",
            AlgoName::Argon2d => "argon2d",
            AlgoName::Unknown => "unknown",
        }
    }
}

/// Inspect a hash string and return its algorithm + parameters.
///
/// Mirrors `Hash::info($hash)`. Returns `HashInfo` with `algo =
/// Unknown` for malformed input — never panics.
pub fn parse(hash: &str) -> HashInfo {
    if let Some(info) = parse_bcrypt(hash) {
        return info;
    }
    if let Some(info) = parse_argon(hash) {
        return info;
    }
    HashInfo {
        algo: AlgoName::Unknown,
        rounds: None,
        memory: None,
        time: None,
        threads: None,
        bcrypt_variant: None,
    }
}

/// True if `value` parses as a recognised algorithm hash.
///
/// Mirrors Laravel's `Hash::isHashed($value)`: anything `info()` can
/// classify returns true. Used by the `AsHashed` eloquent cast to skip
/// re-hashing an already-hashed column.
pub fn is_hashed(value: &str) -> bool {
    !matches!(parse(value).algo, AlgoName::Unknown)
}

/// Parse a bcrypt MCF hash. Accepts the four documented variant prefixes.
///
/// Format: `$2{a,b,x,y}$<cost-2-digits>$<22-char-salt><31-char-digest>`
/// → 60 chars total.
fn parse_bcrypt(hash: &str) -> Option<HashInfo> {
    if hash.len() != 60 {
        return None;
    }
    let variant = if hash.starts_with("$2a$") {
        "2a"
    } else if hash.starts_with("$2b$") {
        "2b"
    } else if hash.starts_with("$2x$") {
        "2x"
    } else if hash.starts_with("$2y$") {
        "2y"
    } else {
        return None;
    };
    // After `$2<X>$` we have `cost$rest`. `cost` is exactly 2 ASCII
    // digits in valid MCF (range 04..=31).
    let body = &hash[4..];
    let (cost_str, rest) = body.split_once('$')?;
    if cost_str.len() != 2 || !cost_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let cost = cost_str.parse::<u32>().ok()?;
    if rest.len() != 53 {
        // 22-char salt + 31-char digest = 53 chars.
        return None;
    }
    Some(HashInfo {
        algo: AlgoName::Bcrypt,
        rounds: Some(cost),
        memory: None,
        time: None,
        threads: None,
        bcrypt_variant: Some(variant),
    })
}

/// Parse an Argon2 PHC hash via the upstream `argon2::PasswordHash`
/// parser. Format:
/// `$argon2{i,id,d}$v=<ver>$m=<mem>,t=<time>,p=<threads>$<salt-b64>$<digest-b64>`.
fn parse_argon(hash: &str) -> Option<HashInfo> {
    use argon2::password_hash::PasswordHash;

    let parsed = PasswordHash::new(hash).ok()?;
    let algo_name = parsed.algorithm.as_str();
    let algo = match algo_name {
        "argon2i" => AlgoName::Argon2i,
        "argon2id" => AlgoName::Argon2id,
        "argon2d" => AlgoName::Argon2d,
        _ => return None,
    };
    // `Params::try_from(&PasswordHash)` decodes the m_cost / t_cost / p_cost
    // values from the PHC string's params section.
    let argon_params = argon2::Params::try_from(&parsed).ok()?;
    Some(HashInfo {
        algo,
        rounds: None,
        memory: Some(argon_params.m_cost()),
        time: Some(argon_params.t_cost()),
        threads: Some(argon_params.p_cost()),
        bcrypt_variant: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_for_empty_and_garbage() {
        assert!(matches!(parse("").algo, AlgoName::Unknown));
        assert!(matches!(parse("not_a_hash").algo, AlgoName::Unknown));
        assert!(!is_hashed(""));
        assert!(!is_hashed("plaintext-password"));
    }

    #[test]
    fn parses_canonical_bcrypt_2b() {
        // bcrypt at cost 4 — generated by hand-known shape.
        let bcrypt = bcrypt::hash("test", 4).expect("hash");
        let info = parse(&bcrypt);
        assert_eq!(info.algo, AlgoName::Bcrypt);
        assert_eq!(info.rounds, Some(4));
        // bcrypt crate emits $2b$ as canonical
        assert_eq!(info.bcrypt_variant, Some("2b"));
        assert!(is_hashed(&bcrypt));
    }

    #[test]
    fn parses_legacy_bcrypt_variants() {
        // Hand-craft 2a / 2x / 2y prefixes by swapping the bcrypt prefix
        // on a real $2b$ hash — the trailing salt+digest is opaque to
        // the parser, so any 60-char $2*$ string parses.
        let base = bcrypt::hash("test", 4).expect("hash");
        for prefix in ["$2a$", "$2x$", "$2y$"] {
            let mut h = String::from(prefix);
            h.push_str(&base[4..]);
            let info = parse(&h);
            assert_eq!(info.algo, AlgoName::Bcrypt);
            assert_eq!(info.rounds, Some(4));
            assert_eq!(info.bcrypt_variant, Some(&prefix[1..3]));
        }
    }

    #[test]
    fn wrong_bcrypt_length_rejected() {
        // 59 chars instead of 60
        let short = "$2b$04$".to_string() + &"a".repeat(52);
        assert!(matches!(parse(&short).algo, AlgoName::Unknown));
    }

    #[test]
    fn parses_argon2id() {
        // Generate a real argon2id hash via the argon2 crate to avoid
        // hand-rolling PHC strings.
        use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
        let salt = SaltString::generate(&mut OsRng);
        let h = argon2::Argon2::default()
            .hash_password(b"test", &salt)
            .unwrap()
            .to_string();
        let info = parse(&h);
        assert_eq!(info.algo, AlgoName::Argon2id);
        assert!(info.memory.is_some());
        assert!(info.time.is_some());
        assert!(info.threads.is_some());
        assert!(is_hashed(&h));
    }

    #[test]
    fn argon_namealias_maps_to_supported() {
        assert_eq!(AlgoName::Bcrypt.supported(), Some(Algorithm::Bcrypt));
        assert_eq!(AlgoName::Argon2i.supported(), Some(Algorithm::Argon2i));
        assert_eq!(AlgoName::Argon2id.supported(), Some(Algorithm::Argon2id));
        assert_eq!(AlgoName::Argon2d.supported(), None);
        assert_eq!(AlgoName::Unknown.supported(), None);
    }
}
