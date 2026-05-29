//! Hashing configuration — env-driven driver selection + per-algo params.
//!
//! Mirrors Laravel's `config/hashing.php` shape on the Rust side. The
//! resolved [`HashConfig`] feeds [`crate::hashing::driver::build`] which
//! returns a `Box<dyn Hasher>` matching the selected algorithm.

use crate::error::FrameworkError;
use std::env;

/// Active hashing algorithm.
///
/// Maps to Laravel's `HASH_DRIVER` env values: `bcrypt`, `argon`
/// (Argon2i), `argon2id`. Case-insensitive on the env-var side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Algorithm {
    /// bcrypt — Laravel's default. Block-cipher-based, has the 72-byte
    /// password ceiling. Output: `$2b$<cost>$<salt><digest>`.
    Bcrypt,
    /// Argon2i — memory-hard, side-channel-resistant. Output:
    /// `$argon2i$v=19$m=…,t=…,p=…$<salt>$<digest>`.
    Argon2i,
    /// Argon2id — hybrid (i + d), the OWASP 2024 recommendation for
    /// password hashing. Output: `$argon2id$v=19$m=…,t=…,p=…$<salt>$<digest>`.
    Argon2id,
}

impl Algorithm {
    /// Stable string label for diagnostics, env-var input, and
    /// algorithm-mismatch error messages. Matches Laravel's
    /// `algoName` values.
    pub fn as_str(&self) -> &'static str {
        match self {
            Algorithm::Bcrypt => "bcrypt",
            Algorithm::Argon2i => "argon2i",
            Algorithm::Argon2id => "argon2id",
        }
    }

    /// Parse an env-side label. Accepts Laravel's spellings — `bcrypt`,
    /// `argon` (alias for `argon2i` — matches `HashManager::createArgonDriver`),
    /// `argon2i`, `argon2id`. Case-insensitive.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bcrypt" => Some(Algorithm::Bcrypt),
            "argon" | "argon2i" => Some(Algorithm::Argon2i),
            "argon2id" => Some(Algorithm::Argon2id),
            _ => None,
        }
    }
}

/// Resolved hashing config: driver + per-algorithm parameters +
/// algorithm-verification gate.
#[derive(Debug, Clone)]
pub struct HashConfig {
    /// Active algorithm. Selected by `HASH_DRIVER`. Default: bcrypt.
    pub driver: Algorithm,
    /// Bcrypt cost / rounds. Range `4..=31`. Default: 12.
    pub rounds: u32,
    /// Argon memory in KiB. Argon-only. Default: 65536 (64 MiB,
    /// OWASP 2024). Minimum: 8.
    pub memory: u32,
    /// Argon time / iterations. Argon-only. Default: 4 (OWASP 2024).
    /// Minimum: 1.
    pub time: u32,
    /// Argon parallelism. Argon-only. Default: 1 (matches OWASP and
    /// libsodium's behaviour). Minimum: 1.
    pub threads: u32,
    /// When true, `verify()` rejects hashes from a different algorithm
    /// (returns `Ok(false)`). Mirrors Laravel's `HASH_VERIFY` env.
    /// Default: false — so legacy bcrypt hashes still verify after a
    /// driver flip until they're rotated.
    pub verify_algorithm: bool,
}

impl Default for HashConfig {
    fn default() -> Self {
        Self {
            driver: Algorithm::Bcrypt,
            rounds: super::DEFAULT_COST,
            memory: 65_536,
            time: 4,
            threads: 1,
            verify_algorithm: false,
        }
    }
}

impl HashConfig {
    /// Resolve config from the process environment.
    ///
    /// Missing env vars fall back to [`HashConfig::default`]. Invalid
    /// values return `FrameworkError::param` with the offending var
    /// name + value so misconfiguration surfaces at first hash, not at
    /// runtime as a silent default.
    pub fn from_env() -> Result<Self, FrameworkError> {
        let mut cfg = HashConfig::default();

        if let Some(s) = env_opt("HASH_DRIVER") {
            cfg.driver = Algorithm::parse(&s).ok_or_else(|| {
                FrameworkError::param(format!(
                    "HASH_DRIVER `{s}` not recognised; expected one of bcrypt, argon, argon2i, argon2id"
                ))
            })?;
        }

        if let Some(s) = env_opt("HASH_ROUNDS") {
            let n = parse_u32("HASH_ROUNDS", &s)?;
            if !(4..=31).contains(&n) {
                return Err(FrameworkError::param(format!(
                    "HASH_ROUNDS={n} out of bcrypt range 4..=31"
                )));
            }
            cfg.rounds = n;
        }

        if let Some(s) = env_opt("HASH_MEMORY") {
            let n = parse_u32("HASH_MEMORY", &s)?;
            if n < 8 {
                return Err(FrameworkError::param(format!(
                    "HASH_MEMORY={n} below argon2 minimum (8 KiB)"
                )));
            }
            cfg.memory = n;
        }

        if let Some(s) = env_opt("HASH_TIME") {
            let n = parse_u32("HASH_TIME", &s)?;
            if n == 0 {
                return Err(FrameworkError::param("HASH_TIME=0 is invalid; minimum 1"));
            }
            cfg.time = n;
        }

        if let Some(s) = env_opt("HASH_THREADS") {
            let n = parse_u32("HASH_THREADS", &s)?;
            if n == 0 {
                return Err(FrameworkError::param(
                    "HASH_THREADS=0 is invalid; minimum 1",
                ));
            }
            cfg.threads = n;
        }

        if let Some(s) = env_opt("HASH_VERIFY") {
            cfg.verify_algorithm = parse_bool("HASH_VERIFY", &s)?;
        }

        Ok(cfg)
    }
}

fn env_opt(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(s) if s.trim().is_empty() => None,
        Ok(s) => Some(s),
        Err(_) => None,
    }
}

fn parse_u32(name: &str, s: &str) -> Result<u32, FrameworkError> {
    s.trim()
        .parse::<u32>()
        .map_err(|e| FrameworkError::param(format!("{name}=`{s}` is not a valid u32: {e}")))
}

fn parse_bool(name: &str, s: &str) -> Result<bool, FrameworkError> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(FrameworkError::param(format!(
            "{name}=`{other}` is not a valid boolean; expected true/false/1/0/yes/no/on/off"
        ))),
    }
}

#[cfg(test)]
pub(super) mod tests {
    //! Env-driven tests — serialised through `ENV_LOCK` because env mutation
    //! is process-wide. Same pattern as `crypto/key.rs`'s `ENV_LOCK`.

    use super::*;
    use std::sync::Mutex;

    /// Serialise every test that pokes env. Public to siblings so the
    /// driver-resolution tests in `driver.rs` can reuse it.
    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Clear every env var this module reads. Used as setup + teardown.
    pub(crate) fn clear_env() {
        for k in [
            "HASH_DRIVER",
            "HASH_ROUNDS",
            "HASH_MEMORY",
            "HASH_TIME",
            "HASH_THREADS",
            "HASH_VERIFY",
        ] {
            // SAFETY: env mutation is process-wide; the ENV_LOCK held by
            // the caller serialises all hashing-env tests within the
            // workspace, and no production code path mutates these vars.
            unsafe { std::env::remove_var(k) };
        }
    }

    #[test]
    fn default_when_no_env() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let cfg = HashConfig::from_env().unwrap();
        assert_eq!(cfg.driver, Algorithm::Bcrypt);
        assert_eq!(cfg.rounds, super::super::DEFAULT_COST);
        assert!(!cfg.verify_algorithm);
    }

    #[test]
    fn parses_each_driver_alias() {
        let _g = ENV_LOCK.lock().unwrap();
        for (val, expect) in [
            ("bcrypt", Algorithm::Bcrypt),
            ("BCRYPT", Algorithm::Bcrypt),
            ("argon", Algorithm::Argon2i),
            ("argon2i", Algorithm::Argon2i),
            ("argon2id", Algorithm::Argon2id),
            ("ARGON2ID", Algorithm::Argon2id),
        ] {
            clear_env();
            unsafe { std::env::set_var("HASH_DRIVER", val) };
            let cfg = HashConfig::from_env().unwrap();
            assert_eq!(cfg.driver, expect, "HASH_DRIVER={val}");
        }
        clear_env();
    }

    #[test]
    fn unknown_driver_rejected() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        unsafe { std::env::set_var("HASH_DRIVER", "scrypt") };
        let err = HashConfig::from_env().unwrap_err();
        assert!(format!("{err}").contains("HASH_DRIVER"));
        clear_env();
    }

    #[test]
    fn rounds_out_of_range() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        unsafe { std::env::set_var("HASH_ROUNDS", "3") };
        let err = HashConfig::from_env().unwrap_err();
        assert!(format!("{err}").contains("HASH_ROUNDS"));
        unsafe { std::env::set_var("HASH_ROUNDS", "32") };
        let err = HashConfig::from_env().unwrap_err();
        assert!(format!("{err}").contains("HASH_ROUNDS"));
        clear_env();
    }

    #[test]
    fn argon_params_picked_up() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        unsafe {
            std::env::set_var("HASH_DRIVER", "argon2id");
            std::env::set_var("HASH_MEMORY", "32768");
            std::env::set_var("HASH_TIME", "2");
            std::env::set_var("HASH_THREADS", "2");
        }
        let cfg = HashConfig::from_env().unwrap();
        assert_eq!(cfg.driver, Algorithm::Argon2id);
        assert_eq!(cfg.memory, 32_768);
        assert_eq!(cfg.time, 2);
        assert_eq!(cfg.threads, 2);
        clear_env();
    }

    #[test]
    fn argon_memory_below_minimum_rejected() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        unsafe { std::env::set_var("HASH_MEMORY", "4") };
        let err = HashConfig::from_env().unwrap_err();
        assert!(format!("{err}").contains("HASH_MEMORY"));
        clear_env();
    }

    #[test]
    fn verify_bool_parses() {
        let _g = ENV_LOCK.lock().unwrap();
        for (val, expect) in [
            ("true", true),
            ("TRUE", true),
            ("1", true),
            ("yes", true),
            ("false", false),
            ("0", false),
            ("no", false),
        ] {
            clear_env();
            unsafe { std::env::set_var("HASH_VERIFY", val) };
            let cfg = HashConfig::from_env().unwrap();
            assert_eq!(cfg.verify_algorithm, expect, "HASH_VERIFY={val}");
        }
        clear_env();
    }
}
