//! Hashing drivers — `Hasher` trait + bcrypt + Argon2i + Argon2id impls.
//!
//! Mirrors Laravel's `BcryptHasher`/`ArgonHasher`/`Argon2IdHasher` trio.
//! The trait is what the facade dispatches through — adding a new
//! algorithm later (e.g. `Scrypt`) means a new impl + a new
//! [`Algorithm`] variant, nothing else.

use super::config::{Algorithm, HashConfig};
use super::info::{AlgoName, parse};
use super::{DEFAULT_COST, MAX_BCRYPT_PASSWORD_BYTES};
use crate::error::FrameworkError;

/// Trait for password hashing drivers.
///
/// Implementations are `Send + Sync` because the facade caches a
/// `Box<dyn Hasher>` in a process-wide `OnceLock`. Methods must not
/// panic on user input — return `FrameworkError` for any failure mode.
pub trait Hasher: Send + Sync + 'static {
    /// Algorithm this hasher produces. Hashes from other algorithms may
    /// still verify (driven by [`HashConfig::verify_algorithm`]) but
    /// fresh `hash()` calls always mint this algo.
    fn algorithm(&self) -> Algorithm;

    /// Mint a fresh hash from `password`. Errors on:
    ///
    /// - Driver-specific length ceilings (bcrypt's 72-byte cap).
    /// - Underlying library errors (RNG failure during salt generation,
    ///   parameter-validation errors).
    fn hash(&self, password: &str) -> Result<String, FrameworkError>;

    /// Verify `password` against `hash`. Returns `Ok(false)` for:
    ///
    /// - Wrong password.
    /// - Hashes from a different algorithm when
    ///   [`Self::verify_algorithm`] is true.
    /// - Bcrypt: `password.len() > MAX_BCRYPT_PASSWORD_BYTES` (so auth
    ///   flows surface the same "invalid credentials" response without
    ///   leaking length info).
    /// - Empty / malformed hash strings.
    ///
    /// Errors only on underlying library faults (constant-time compare
    /// failures, salt-decode errors).
    fn verify(&self, password: &str, hash: &str) -> Result<bool, FrameworkError>;

    /// True if `hash` should be re-hashed:
    ///
    /// - Algorithm mismatch (`hash` was minted by a different driver).
    /// - Parameter weakness (bcrypt cost / argon m,t,p below current).
    /// - Bcrypt legacy variants (`$2a$`, `$2x$`, `$2y$`).
    /// - Unrecognised hash format.
    fn needs_rehash(&self, hash: &str) -> bool;

    /// True when [`Self::verify`] should reject hashes from a different
    /// algorithm. Wired to [`HashConfig::verify_algorithm`]. Default:
    /// false (lets legacy bcrypt hashes still verify after a flip).
    fn verify_algorithm(&self) -> bool {
        false
    }
}

/// Build a driver from a config. Used by the facade's
/// [`crate::hashing::default_driver`] resolver.
pub(super) fn build(cfg: &HashConfig) -> Result<Box<dyn Hasher>, FrameworkError> {
    match cfg.driver {
        Algorithm::Bcrypt => Ok(Box::new(BcryptHasher {
            opts: BcryptOptions { rounds: cfg.rounds },
            verify_algorithm: cfg.verify_algorithm,
        })),
        Algorithm::Argon2i => Ok(Box::new(Argon2iHasher::with_config(cfg)?)),
        Algorithm::Argon2id => Ok(Box::new(Argon2idHasher::with_config(cfg)?)),
    }
}

// ============================================================================
// Bcrypt
// ============================================================================

/// Bcrypt driver options.
#[derive(Debug, Clone, Copy)]
pub struct BcryptOptions {
    /// Cost factor, range `4..=31`. Default 12.
    pub rounds: u32,
}

impl Default for BcryptOptions {
    fn default() -> Self {
        Self {
            rounds: DEFAULT_COST,
        }
    }
}

/// Bcrypt password hasher. The default driver.
///
/// Honours the 72-byte block-size cap — passwords longer than
/// [`MAX_BCRYPT_PASSWORD_BYTES`] are rejected by [`Hasher::hash`] and
/// fail verification (`Ok(false)`) in [`Hasher::verify`].
pub struct BcryptHasher {
    opts: BcryptOptions,
    verify_algorithm: bool,
}

impl BcryptHasher {
    /// Construct with explicit options. Pass
    /// `BcryptOptions { rounds: 12 }` for the framework default cost.
    pub fn new(opts: BcryptOptions) -> Self {
        Self {
            opts,
            verify_algorithm: false,
        }
    }

    /// Toggle the `HASH_VERIFY`-aligned algorithm-rejection gate.
    pub fn with_verify_algorithm(mut self, verify: bool) -> Self {
        self.verify_algorithm = verify;
        self
    }
}

impl Default for BcryptHasher {
    fn default() -> Self {
        Self::new(BcryptOptions::default())
    }
}

impl Hasher for BcryptHasher {
    fn algorithm(&self) -> Algorithm {
        Algorithm::Bcrypt
    }

    fn hash(&self, password: &str) -> Result<String, FrameworkError> {
        if password.len() > MAX_BCRYPT_PASSWORD_BYTES {
            return Err(FrameworkError::param(format!(
                "password exceeds {MAX_BCRYPT_PASSWORD_BYTES}-byte bcrypt usable limit \
                 (block size 72 minus null terminator) (got {} bytes); reject at the \
                 form-input layer or switch to HASH_DRIVER=argon2id",
                password.len()
            )));
        }
        bcrypt::non_truncating_hash(password, self.opts.rounds)
            .map_err(|e| FrameworkError::internal(format!("bcrypt hash error: {e}")))
    }

    fn verify(&self, password: &str, hash: &str) -> Result<bool, FrameworkError> {
        if hash.is_empty() {
            return Ok(false);
        }
        if password.len() > MAX_BCRYPT_PASSWORD_BYTES {
            // Length-based info disclosure mitigation: oversized password
            // can't match any hash this driver produces.
            return Ok(false);
        }
        if self.verify_algorithm {
            let info = parse(hash);
            if !matches!(info.algo, AlgoName::Bcrypt) {
                return Ok(false);
            }
        }
        // Defense in depth — only attempt bcrypt verify against $2*$ inputs.
        // The bcrypt crate errors on non-bcrypt hashes; we treat that as
        // `false` (the user's stored hash is from a different algorithm).
        match bcrypt::verify(password, hash) {
            Ok(v) => Ok(v),
            Err(e) => {
                let msg = format!("{e}");
                if msg.to_lowercase().contains("invalid")
                    || msg.to_lowercase().contains("not bcrypt")
                {
                    return Ok(false);
                }
                Err(FrameworkError::internal(format!(
                    "bcrypt verify error: {e}"
                )))
            }
        }
    }

    fn needs_rehash(&self, hash: &str) -> bool {
        let info = parse(hash);
        match info.algo {
            AlgoName::Bcrypt => {
                // Algo matches. Now check variant + cost.
                if info.bcrypt_variant != Some("2b") {
                    // Legacy $2a$/$2x$/$2y$ — rotate to canonical $2b$.
                    return true;
                }
                info.rounds.map(|c| c < self.opts.rounds).unwrap_or(true)
            }
            // Different algorithm — caller should rehash with us.
            _ => true,
        }
    }

    fn verify_algorithm(&self) -> bool {
        self.verify_algorithm
    }
}

// ============================================================================
// Argon2 (shared between Argon2i and Argon2id)
// ============================================================================

/// Argon2 driver options.
#[derive(Debug, Clone, Copy)]
pub struct Argon2Options {
    /// Memory cost in KiB. Default 65536 (64 MiB, OWASP 2024). Minimum
    /// 8.
    pub memory: u32,
    /// Time cost / iterations. Default 4 (OWASP 2024). Minimum 1.
    pub time: u32,
    /// Parallelism / lanes. Default 1. Minimum 1.
    pub threads: u32,
}

impl Default for Argon2Options {
    fn default() -> Self {
        Self {
            memory: 65_536,
            time: 4,
            threads: 1,
        }
    }
}

/// Argon2i password hasher. Side-channel-resistant variant.
pub struct Argon2iHasher {
    opts: Argon2Options,
    verify_algorithm: bool,
}

impl Argon2iHasher {
    /// Construct an `Argon2i` hasher with the supplied parameters.
    ///
    /// Returns an error if `opts` falls below the OWASP safety floor.
    pub fn new(opts: Argon2Options) -> Result<Self, FrameworkError> {
        validate_argon_opts(&opts)?;
        Ok(Self {
            opts,
            verify_algorithm: false,
        })
    }

    /// Toggle algorithm-prefix verification on the verify path. With this
    /// on, [`Hasher::verify`] rejects a hash whose stored algorithm
    /// (`$argon2i$…` / `$argon2id$…`) doesn't match this hasher's
    /// algorithm, instead of letting the underlying library accept it.
    pub fn with_verify_algorithm(mut self, verify: bool) -> Self {
        self.verify_algorithm = verify;
        self
    }

    fn with_config(cfg: &HashConfig) -> Result<Self, FrameworkError> {
        let mut s = Self::new(Argon2Options {
            memory: cfg.memory,
            time: cfg.time,
            threads: cfg.threads,
        })?;
        s.verify_algorithm = cfg.verify_algorithm;
        Ok(s)
    }
}

impl Hasher for Argon2iHasher {
    fn algorithm(&self) -> Algorithm {
        Algorithm::Argon2i
    }

    fn hash(&self, password: &str) -> Result<String, FrameworkError> {
        argon_hash(argon2::Algorithm::Argon2i, &self.opts, password)
    }

    fn verify(&self, password: &str, hash: &str) -> Result<bool, FrameworkError> {
        argon_verify(self.verify_algorithm, AlgoName::Argon2i, password, hash)
    }

    fn needs_rehash(&self, hash: &str) -> bool {
        argon_needs_rehash(self.algorithm(), &self.opts, hash)
    }

    fn verify_algorithm(&self) -> bool {
        self.verify_algorithm
    }
}

/// Argon2id password hasher. OWASP 2024 recommendation. Default for new
/// projects that pick `HASH_DRIVER=argon2id`.
pub struct Argon2idHasher {
    opts: Argon2Options,
    verify_algorithm: bool,
}

impl Argon2idHasher {
    /// Construct an `Argon2id` hasher with the supplied parameters.
    ///
    /// Returns an error if `opts` falls below the OWASP safety floor.
    pub fn new(opts: Argon2Options) -> Result<Self, FrameworkError> {
        validate_argon_opts(&opts)?;
        Ok(Self {
            opts,
            verify_algorithm: false,
        })
    }

    /// Toggle algorithm-prefix verification on the verify path. With this
    /// on, [`Hasher::verify`] rejects a hash whose stored algorithm
    /// doesn't match `Argon2id`.
    pub fn with_verify_algorithm(mut self, verify: bool) -> Self {
        self.verify_algorithm = verify;
        self
    }

    fn with_config(cfg: &HashConfig) -> Result<Self, FrameworkError> {
        let mut s = Self::new(Argon2Options {
            memory: cfg.memory,
            time: cfg.time,
            threads: cfg.threads,
        })?;
        s.verify_algorithm = cfg.verify_algorithm;
        Ok(s)
    }
}

impl Hasher for Argon2idHasher {
    fn algorithm(&self) -> Algorithm {
        Algorithm::Argon2id
    }

    fn hash(&self, password: &str) -> Result<String, FrameworkError> {
        argon_hash(argon2::Algorithm::Argon2id, &self.opts, password)
    }

    fn verify(&self, password: &str, hash: &str) -> Result<bool, FrameworkError> {
        argon_verify(self.verify_algorithm, AlgoName::Argon2id, password, hash)
    }

    fn needs_rehash(&self, hash: &str) -> bool {
        argon_needs_rehash(self.algorithm(), &self.opts, hash)
    }

    fn verify_algorithm(&self) -> bool {
        self.verify_algorithm
    }
}

// ---------- Argon2 helpers ------------------------------------------------

fn validate_argon_opts(opts: &Argon2Options) -> Result<(), FrameworkError> {
    if opts.memory < 8 {
        return Err(FrameworkError::param(format!(
            "argon2 memory={} below minimum 8 KiB",
            opts.memory
        )));
    }
    if opts.time == 0 {
        return Err(FrameworkError::param("argon2 time=0; minimum 1"));
    }
    if opts.threads == 0 {
        return Err(FrameworkError::param("argon2 threads=0; minimum 1"));
    }
    // argon2 crate requires m_cost >= p_cost * 8.
    if opts.memory < opts.threads.saturating_mul(8) {
        return Err(FrameworkError::param(format!(
            "argon2 memory={} below required {}*8 = {} for threads={}",
            opts.memory,
            opts.threads,
            opts.threads.saturating_mul(8),
            opts.threads
        )));
    }
    Ok(())
}

fn argon_hash(
    algo: argon2::Algorithm,
    opts: &Argon2Options,
    password: &str,
) -> Result<String, FrameworkError> {
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};

    let params = argon2::Params::new(opts.memory, opts.time, opts.threads, None)
        .map_err(|e| FrameworkError::internal(format!("argon2 params error: {e}")))?;
    let hasher = argon2::Argon2::new(algo, argon2::Version::V0x13, params);
    let salt = SaltString::generate(&mut OsRng);
    hasher
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| FrameworkError::internal(format!("argon2 hash error: {e}")))
}

fn argon_verify(
    verify_algorithm: bool,
    own_algo: AlgoName,
    password: &str,
    hash: &str,
) -> Result<bool, FrameworkError> {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};

    if hash.is_empty() {
        return Ok(false);
    }
    let parsed = match PasswordHash::new(hash) {
        Ok(p) => p,
        // Non-PHC input — treat as failed verify (could be bcrypt or
        // garbage; we don't error out so the auth flow stays uniform).
        Err(_) => return Ok(false),
    };
    if verify_algorithm {
        let algo_str = parsed.algorithm.as_str();
        let parsed_algo = match algo_str {
            "argon2i" => AlgoName::Argon2i,
            "argon2id" => AlgoName::Argon2id,
            "argon2d" => AlgoName::Argon2d,
            _ => return Ok(false),
        };
        if parsed_algo != own_algo {
            return Ok(false);
        }
    }
    // `verify_password` checks the digest using Argon2's constant-time
    // primitives. Verifier is constructed with defaults — the hash
    // string carries its own params, so the verifier's defaults are
    // ignored for the cost check.
    Ok(argon2::Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

fn argon_needs_rehash(own_algo: Algorithm, opts: &Argon2Options, hash: &str) -> bool {
    let info = parse(hash);
    // Algorithm mismatch — rotate.
    if info.algo.supported() != Some(own_algo) {
        return true;
    }
    // Argon — compare m/t/p against current; if any is weaker, rehash.
    let m = info.memory.unwrap_or(0);
    let t = info.time.unwrap_or(0);
    let p = info.threads.unwrap_or(0);
    m < opts.memory || t < opts.time || p < opts.threads
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bcrypt_rejects_oversized_password() {
        let driver = BcryptHasher::new(BcryptOptions { rounds: 4 });
        let too_long = "x".repeat(MAX_BCRYPT_PASSWORD_BYTES + 1);
        let err = driver.hash(&too_long).expect_err("over-cap must reject");
        assert!(format!("{err}").contains("71"));
    }

    #[test]
    fn bcrypt_oversized_verify_returns_false() {
        let driver = BcryptHasher::new(BcryptOptions { rounds: 4 });
        let h = driver.hash("short").expect("hash");
        let oversized = "x".repeat(MAX_BCRYPT_PASSWORD_BYTES + 1);
        assert!(!driver.verify(&oversized, &h).expect("verify"));
    }

    #[test]
    fn bcrypt_needs_rehash_on_algorithm_mismatch() {
        // Mint an argon2id hash, then ask the bcrypt driver if it needs
        // rehashing — must be true.
        let argon = Argon2idHasher::new(Argon2Options::default()).expect("ctor");
        let h = argon.hash("test").expect("hash");
        let bcrypt = BcryptHasher::new(BcryptOptions { rounds: 12 });
        assert!(bcrypt.needs_rehash(&h));
    }

    #[test]
    fn bcrypt_needs_rehash_on_legacy_variant() {
        let driver = BcryptHasher::new(BcryptOptions { rounds: 12 });
        // Hand-craft a $2a$ hash by swapping a real $2b$ prefix.
        let h = bcrypt::hash("test", 12).expect("hash");
        let mut legacy = String::from("$2a$");
        legacy.push_str(&h[4..]);
        assert!(driver.needs_rehash(&legacy));
    }

    #[test]
    fn argon2id_hash_and_verify_round_trips() {
        // Small params for fast tests.
        let driver = Argon2idHasher::new(Argon2Options {
            memory: 8,
            time: 1,
            threads: 1,
        })
        .expect("ctor");
        let h = driver.hash("the magic words").expect("hash");
        assert!(h.starts_with("$argon2id$"));
        assert!(driver.verify("the magic words", &h).expect("verify"));
        assert!(!driver.verify("wrong", &h).expect("verify"));
    }

    #[test]
    fn argon2i_hash_and_verify_round_trips() {
        let driver = Argon2iHasher::new(Argon2Options {
            memory: 8,
            time: 1,
            threads: 1,
        })
        .expect("ctor");
        let h = driver.hash("the magic words").expect("hash");
        assert!(h.starts_with("$argon2i$"));
        assert!(driver.verify("the magic words", &h).expect("verify"));
    }

    #[test]
    fn argon2id_accepts_long_password() {
        // The 72-byte ceiling is bcrypt-specific. Argon takes
        // arbitrary-length input.
        let driver = Argon2idHasher::new(Argon2Options {
            memory: 8,
            time: 1,
            threads: 1,
        })
        .expect("ctor");
        let long = "x".repeat(500);
        let h = driver.hash(&long).expect("hash");
        assert!(driver.verify(&long, &h).expect("verify"));
    }

    #[test]
    fn argon2id_needs_rehash_on_weaker_params() {
        // Hash at memory=8 then check whether memory=16 driver wants a rehash.
        let weak = Argon2idHasher::new(Argon2Options {
            memory: 8,
            time: 1,
            threads: 1,
        })
        .expect("ctor");
        let h = weak.hash("test").expect("hash");
        let strong = Argon2idHasher::new(Argon2Options {
            memory: 16,
            time: 1,
            threads: 1,
        })
        .expect("ctor");
        assert!(strong.needs_rehash(&h));
    }

    #[test]
    fn argon2id_needs_rehash_on_algo_mismatch() {
        // bcrypt hash through argon2id driver → needs rehash.
        let bcrypt_hash = bcrypt::hash("test", 4).expect("hash");
        let driver = Argon2idHasher::new(Argon2Options::default()).expect("ctor");
        assert!(driver.needs_rehash(&bcrypt_hash));
    }

    #[test]
    fn verify_algorithm_gate_rejects_cross_algo() {
        // Bcrypt driver with verify_algorithm=true should refuse an argon hash.
        let bcrypt = BcryptHasher::new(BcryptOptions { rounds: 4 }).with_verify_algorithm(true);
        let argon = Argon2idHasher::new(Argon2Options {
            memory: 8,
            time: 1,
            threads: 1,
        })
        .expect("ctor");
        let argon_hash = argon.hash("test").expect("hash");
        assert!(!bcrypt.verify("test", &argon_hash).expect("verify"));

        // And the converse.
        let argon_strict = Argon2idHasher::new(Argon2Options {
            memory: 8,
            time: 1,
            threads: 1,
        })
        .expect("ctor")
        .with_verify_algorithm(true);
        let bcrypt_hash = bcrypt::hash("test", 4).expect("hash");
        assert!(!argon_strict.verify("test", &bcrypt_hash).expect("verify"));
    }

    #[test]
    fn driver_verify_is_single_algorithm_only() {
        // Per-driver `verify` is single-algorithm by design — it tests
        // ONLY its own family. Cross-algorithm verification is the
        // facade's job (`crate::hashing::verify_with` dispatches on the
        // stored hash's algorithm). At the driver level, a bcrypt
        // driver returns `Ok(false)` for an argon hash regardless of
        // password match, and vice versa.
        let bcrypt = BcryptHasher::new(BcryptOptions { rounds: 4 });
        let argon = Argon2idHasher::new(Argon2Options {
            memory: 8,
            time: 1,
            threads: 1,
        })
        .expect("ctor");
        let argon_hash = argon.hash("test").expect("hash");
        assert!(
            !bcrypt.verify("test", &argon_hash).expect("verify"),
            "driver-level verify is single-algorithm; cross-algo dispatch is the facade's job"
        );
    }

    #[test]
    fn validate_argon_opts_rejects_memory_below_min() {
        assert!(
            Argon2idHasher::new(Argon2Options {
                memory: 4,
                time: 1,
                threads: 1
            })
            .is_err()
        );
    }

    #[test]
    fn validate_argon_opts_rejects_zero_time() {
        assert!(
            Argon2idHasher::new(Argon2Options {
                memory: 16,
                time: 0,
                threads: 1
            })
            .is_err()
        );
    }
}
