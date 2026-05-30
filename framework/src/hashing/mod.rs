//! Password hashing for suprnova framework.
//!
//! Parity with Laravel 13's [`Hash` facade]
//! (`reference/framework-13.9.0/src/Illuminate/Hashing/`): three drivers
//! ([`BcryptHasher`], [`Argon2iHasher`], [`Argon2idHasher`]), env-driven
//! driver selection (`HASH_DRIVER`), algorithm-aware [`needs_rehash`] /
//! [`info`] / [`is_hashed`], and an algorithm-verification gate on
//! [`verify`] (`HASH_VERIFY`).
//!
//! # Async-safe vs sync facades
//!
//! Both bcrypt at cost 12 (~250 ms) and Argon2id at memory=64 MiB / time=4
//! (~80 ms) are intentionally CPU-bound. Calling [`hash`] / [`verify`]
//! directly from a Tokio request handler blocks the worker thread for the
//! whole duration. Use the [`hash_async`] / [`verify_async`] siblings
//! inside `async fn` handlers — they dispatch onto `spawn_blocking` so the
//! worker stays free for other requests. The sync variants stay for tests,
//! CLI tools, and other non-async call sites.
//!
//! # Algorithm-aware length guard
//!
//! Bcrypt's internal block size limits passwords to 72 bytes — the `bcrypt`
//! crate's `hash` / `verify` functions silently truncate longer inputs,
//! which means two distinct passphrases sharing their first 72 bytes hash
//! to the same value (audit HIGH `hashing` #2). When the active driver is
//! bcrypt, [`hash`] rejects passwords > [`MAX_BCRYPT_PASSWORD_BYTES`]
//! up-front:
//!
//! - [`hash`] returns `FrameworkError::param("password exceeds … bytes")`.
//! - [`verify`] returns `Ok(false)` so the calling auth flow surfaces the
//!   same "invalid credentials" response regardless — no length-based
//!   information disclosure.
//!
//! Argon2i / Argon2id have **no such limit** and accept arbitrary-length
//! passphrases. The guard only fires when the active driver is bcrypt.
//!
//! # Configuration
//!
//! Three env vars select and tune the driver — see [`HashConfig`] for the
//! resolved shape:
//!
//! | Env var | Default | Range |
//! |---------|---------|-------|
//! | `HASH_DRIVER` | `bcrypt` | `bcrypt` \| `argon` \| `argon2id` |
//! | `HASH_ROUNDS` | `12` | `4..=31` (bcrypt only) |
//! | `HASH_MEMORY` | `65536` KiB | argon only; `>= 8` |
//! | `HASH_TIME` | `4` | argon only; `>= 1` |
//! | `HASH_THREADS` | `1` | argon only; `>= 1` |
//! | `HASH_VERIFY` | `false` | when `true`, [`verify`] rejects hashes from a different algorithm |
//!
//! Suprnova's argon defaults match the OWASP 2024 recommendation
//! (`m = 64 MiB, t = 4, p = 1`) — stronger than Laravel's PHP defaults
//! (`m = 1024 KiB, t = 2, p = 2`) because Rust workers can spend the
//! memory.
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::hashing;
//!
//! // Async (preferred inside request handlers):
//! let hash = hashing::hash_async("my_password").await?;
//! let valid = hashing::verify_async("my_password", &hash).await?;
//!
//! // Sync (tests, CLI tools, non-async contexts):
//! let hash = hashing::hash("my_password")?;
//! let valid = hashing::verify("my_password", &hash)?;
//!
//! // Algorithm-aware rotation:
//! if hashing::needs_rehash(&stored_hash) {
//!     let fresh = hashing::hash_async("my_password").await?;
//!     // persist `fresh` in place of `stored_hash`
//! }
//! ```

use crate::error::FrameworkError;
use std::sync::OnceLock;

mod config;
mod driver;
mod info;

pub use config::{Algorithm, HashConfig};
pub use driver::{
    Argon2Options, Argon2iHasher, Argon2idHasher, BcryptHasher, BcryptOptions, Hasher,
};
pub use info::{AlgoName, HashInfo, is_hashed, parse};

/// Default bcrypt cost factor (matches Laravel 13).
pub const DEFAULT_COST: u32 = 12;

/// Default bcrypt cost factor (Laravel-side alias for [`DEFAULT_COST`]).
pub const DEFAULT_ROUNDS: u32 = DEFAULT_COST;

/// Minimum bcrypt cost accepted by [`hash_with_cost`]. Matches the bcrypt
/// crate's `MIN_COST` and the `HASH_ROUNDS` env-side floor.
pub const MIN_BCRYPT_COST: u32 = 4;

/// Maximum bcrypt cost accepted by [`hash_with_cost`]. Matches the bcrypt
/// crate's `MAX_COST` and the `HASH_ROUNDS` env-side ceiling. Each
/// increment doubles CPU time — at cost 31 a single hash takes hours on
/// commodity hardware, which is why route code that flows policy/config
/// values into [`hash_with_cost`] gets bounds-checked here rather than
/// relying on the upstream crate's range check.
pub const MAX_BCRYPT_COST: u32 = 31;

/// Maximum usable password length when the active driver is bcrypt.
///
/// Bcrypt requires a trailing null byte inside its 72-byte block, so the
/// usable password limit is 71 bytes — `non_truncating_hash` itself errors
/// with `"Expected 72 bytes or fewer; found 73 bytes"` when handed exactly
/// 72 password bytes. Suprnova rejects up-front to prevent two distinct
/// passphrases sharing the same first 71 bytes from authenticating as the
/// same password (audit HIGH `hashing` #2). The Argon2 drivers have no
/// equivalent ceiling.
pub const MAX_BCRYPT_PASSWORD_BYTES: usize = 71;

/// Legacy alias for [`MAX_BCRYPT_PASSWORD_BYTES`]. Pre-Argon2 callers used
/// this name; keep it for source compatibility.
pub const MAX_PASSWORD_BYTES: usize = MAX_BCRYPT_PASSWORD_BYTES;

static DEFAULT_DRIVER: OnceLock<Box<dyn Hasher>> = OnceLock::new();

/// Resolve the active hasher driver.
///
/// First call initialises the driver from the process environment via
/// [`HashConfig::from_env`]; subsequent calls return the cached instance.
/// Configuration errors propagate; callers see a concrete error instead
/// of a panic.
pub fn default_driver() -> Result<&'static dyn Hasher, FrameworkError> {
    if let Some(d) = DEFAULT_DRIVER.get() {
        return Ok(d.as_ref());
    }
    let cfg = HashConfig::from_env()?;
    let driver = driver::build(&cfg)?;
    // Race-safe: OnceLock::set returns Err if another thread initialised
    // first; both drivers were built from the same env, so we just
    // discard ours and return the winner.
    let _ = DEFAULT_DRIVER.set(driver);
    Ok(DEFAULT_DRIVER
        .get()
        .expect("DEFAULT_DRIVER initialised above")
        .as_ref())
}

/// Override the active hasher driver. Intended for tests and embedded
/// CLI tools that build the driver programmatically rather than from env.
///
/// Returns `Err(FrameworkError::internal(...))` if the driver was already
/// initialised — by design, the active driver does not flip mid-process.
pub fn set_default_driver(driver: Box<dyn Hasher>) -> Result<(), FrameworkError> {
    DEFAULT_DRIVER.set(driver).map_err(|_| {
        FrameworkError::internal(
            "hashing: default driver already initialised; cannot override after first use",
        )
    })
}

/// Hash a password using the active driver.
///
/// **Synchronous** — blocks the calling thread for ~250 ms (bcrypt) or
/// ~80 ms (argon2id default). Use [`hash_async`] inside Tokio request
/// handlers.
///
/// When the active driver is bcrypt, returns `FrameworkError::param` if
/// `password` exceeds [`MAX_BCRYPT_PASSWORD_BYTES`] — see module docs for
/// the rationale.
pub fn hash(password: &str) -> Result<String, FrameworkError> {
    let driver = default_driver()?;
    hash_with(driver, password)
}

/// Hash a password using an explicit driver. Used by tests and by the
/// facade above.
pub fn hash_with(driver: &dyn Hasher, password: &str) -> Result<String, FrameworkError> {
    driver.hash(password)
}

/// Hash a password using bcrypt with a caller-supplied cost factor.
///
/// **Bcrypt-specific.** This bypasses driver selection and uses bcrypt
/// regardless of `HASH_DRIVER`. Use [`hash`] for the configured driver.
///
/// **Synchronous** — see [`hash_with_cost_async`] for the async-safe
/// variant.
///
/// Rejects `cost` outside [`MIN_BCRYPT_COST`]`..=`[`MAX_BCRYPT_COST`]
/// with [`FrameworkError::param`]. Mirrors the env-side `HASH_ROUNDS`
/// validation so route code that flows policy/config values into this
/// entry point can't accidentally request a cost so high it pins a
/// worker thread for hours.
pub fn hash_with_cost(password: &str, cost: u32) -> Result<String, FrameworkError> {
    if !(MIN_BCRYPT_COST..=MAX_BCRYPT_COST).contains(&cost) {
        return Err(FrameworkError::param(format!(
            "bcrypt cost={cost} out of range {MIN_BCRYPT_COST}..={MAX_BCRYPT_COST}"
        )));
    }
    let bcrypt = BcryptHasher::new(BcryptOptions { rounds: cost });
    bcrypt.hash(password)
}

/// Verify a password against a hash.
///
/// **Stored-algorithm-aware.** Dispatch is on the hash's algorithm, not
/// the configured driver — same shape as PHP's `password_verify`. This is
/// what enables live migration from bcrypt → argon2id: existing bcrypt
/// hashes still verify after a `HASH_DRIVER` flip so callers can rotate
/// them on the next successful login via [`needs_rehash`].
///
/// **Synchronous** — see [`verify_async`] for the async-safe variant.
/// Uses constant-time comparison (delegated to the underlying crate) to
/// prevent timing attacks.
///
/// For bcrypt, a password longer than [`MAX_BCRYPT_PASSWORD_BYTES`]
/// cannot match any hash this module produces, so verify returns
/// `Ok(false)` rather than an error — keeps the calling auth flow
/// returning the same "invalid credentials" response regardless of
/// length.
///
/// When `HASH_VERIFY=true` AND the configured driver's algorithm
/// differs from the stored hash's algorithm, [`verify`] returns
/// `Ok(false)`. Set `HASH_VERIFY=false` (the default) while rotating
/// from bcrypt → argon2id so legacy hashes still match.
pub fn verify(password: &str, hash: &str) -> Result<bool, FrameworkError> {
    let driver = default_driver()?;
    verify_with(driver, password, hash)
}

/// Verify a password against a hash. Dispatch is on the hash's
/// algorithm; `configured_driver` is consulted only to apply the
/// `HASH_VERIFY` cross-algorithm rejection gate.
///
/// Used by tests and by the facade above.
pub fn verify_with(
    configured_driver: &dyn Hasher,
    password: &str,
    hash: &str,
) -> Result<bool, FrameworkError> {
    if hash.is_empty() {
        return Ok(false);
    }

    let stored = info::parse(hash);

    // `HASH_VERIFY` cross-algorithm rejection gate. Compare the stored
    // algorithm against the configured driver's algorithm and reject if
    // they differ. Apply at the facade so the underlying verify still
    // dispatches on the stored algo regardless.
    if configured_driver.verify_algorithm() {
        let stored_algo = stored.algo.supported();
        if stored_algo != Some(configured_driver.algorithm()) {
            return Ok(false);
        }
    }

    // Dispatch on the stored algorithm, not the configured driver.
    // Within an algorithm family, params come from the hash string —
    // a default-param verifier of the right family suffices because
    // bcrypt::verify reads cost from `$2*$cost$…` and
    // `Argon2::default().verify_password` reads m/t/p from the PHC
    // string.
    match stored.algo {
        info::AlgoName::Bcrypt => {
            // Bcrypt's 72-byte length guard applies on the verify side
            // too — a >71-byte password cannot match any bcrypt hash
            // this module produces.
            if password.len() > MAX_BCRYPT_PASSWORD_BYTES {
                return Ok(false);
            }
            verify_bcrypt(password, hash)
        }
        info::AlgoName::Argon2i | info::AlgoName::Argon2id | info::AlgoName::Argon2d => {
            verify_argon(password, hash)
        }
        info::AlgoName::Unknown => {
            // Stored hash is in no recognised algorithm — fall back to
            // the configured driver's verify (which will also return
            // false, but preserves any custom behaviour a user-supplied
            // driver might add).
            configured_driver.verify(password, hash)
        }
    }
}

/// Verify against a bcrypt MCF hash. Cost comes from the hash string,
/// so we don't need a configured driver instance here.
fn verify_bcrypt(password: &str, hash: &str) -> Result<bool, FrameworkError> {
    match bcrypt::verify(password, hash) {
        Ok(v) => Ok(v),
        Err(e) => {
            // bcrypt::verify errors on non-bcrypt input. Since
            // `verify_with` already routed by stored algo, we only land
            // here on a legitimately bcrypt-shaped hash — error means
            // corrupted hash, not a different algorithm.
            let msg = format!("{e}");
            let lower = msg.to_lowercase();
            if lower.contains("invalid") || lower.contains("not bcrypt") {
                return Ok(false);
            }
            Err(FrameworkError::internal(format!(
                "bcrypt verify error: {e}"
            )))
        }
    }
}

/// Verify against an Argon2 PHC hash. Params come from the hash string.
fn verify_argon(password: &str, hash: &str) -> Result<bool, FrameworkError> {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};

    let parsed = match PasswordHash::new(hash) {
        Ok(p) => p,
        Err(_) => return Ok(false),
    };
    Ok(argon2::Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// Async-safe wrapper around [`hash`]. Runs the CPU-bound hash on
/// `tokio::task::spawn_blocking` so the calling worker thread stays free.
pub async fn hash_async(password: &str) -> Result<String, FrameworkError> {
    let pw = password.to_string();
    tokio::task::spawn_blocking(move || hash(&pw))
        .await
        .map_err(|e| FrameworkError::internal(format!("hash_async join error: {e}")))?
}

/// Async-safe wrapper around [`hash_with_cost`]. Bcrypt-specific.
pub async fn hash_with_cost_async(password: &str, cost: u32) -> Result<String, FrameworkError> {
    let pw = password.to_string();
    tokio::task::spawn_blocking(move || hash_with_cost(&pw, cost))
        .await
        .map_err(|e| FrameworkError::internal(format!("hash_with_cost_async join error: {e}")))?
}

/// Async-safe wrapper around [`verify`].
pub async fn verify_async(password: &str, hash: &str) -> Result<bool, FrameworkError> {
    let pw = password.to_string();
    let h = hash.to_string();
    tokio::task::spawn_blocking(move || verify(&pw, &h))
        .await
        .map_err(|e| FrameworkError::internal(format!("verify_async join error: {e}")))?
}

/// True if `hash` was produced with weaker parameters than the active
/// driver would mint today, or by a different algorithm.
///
/// Mirrors Laravel's `Hash::needsRehash`:
/// `password_needs_rehash($hashed, PASSWORD_BCRYPT, ['cost' => 12])`.
/// Suprnova's check covers both axes:
///
/// - **Algorithm mismatch.** If the hash's algorithm differs from the
///   configured driver (e.g. stored as bcrypt while `HASH_DRIVER=argon2id`),
///   the hash needs a fresh hash under the new algorithm.
/// - **Parameter weakness.** If the hash's params (bcrypt cost, argon
///   memory/time/threads) are below the configured values, rehash to
///   bring the stored hash up to current strength.
///
/// The bcrypt path additionally recognises legacy variants (`$2a$`,
/// `$2x$`, `$2y$`) and treats them as needing rehash even at the
/// configured cost, matching `password_needs_rehash`'s behaviour.
///
/// Returns `true` for malformed input so the caller naturally rotates
/// any hash it can't parse.
pub fn needs_rehash(hash: &str) -> bool {
    let Ok(driver) = default_driver() else {
        return true;
    };
    needs_rehash_with(driver, hash)
}

/// Like [`needs_rehash`] but takes an explicit driver. Used by tests and
/// the facade.
pub fn needs_rehash_with(driver: &dyn Hasher, hash: &str) -> bool {
    driver.needs_rehash(hash)
}

/// Inspect a hash and return its algorithm + parameters. Returns
/// `HashInfo { algo: Algorithm::Unknown, .. }` for inputs that don't
/// match any recognised hash format.
///
/// Equivalent to Laravel's `Hash::info($hash)`. Useful in migration
/// scripts ("how many users are still on bcrypt?") and in custom
/// [`needs_rehash`] policies.
pub fn info(hash: &str) -> HashInfo {
    info::parse(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience: build a bcrypt driver at the framework default cost
    /// without touching the global static. Tests should use this to
    /// avoid races with the global `DEFAULT_DRIVER` cell.
    fn bcrypt_default() -> BcryptHasher {
        BcryptHasher::new(BcryptOptions {
            rounds: DEFAULT_COST,
        })
    }

    #[test]
    fn bcrypt_hash_and_verify() {
        let driver = bcrypt_default();
        let password = "test_password_123";
        let hashed = hash_with(&driver, password).expect("hash");
        assert!(hashed.starts_with("$2"));
        assert!(verify_with(&driver, password, &hashed).expect("verify"));
        assert!(!verify_with(&driver, "wrong", &hashed).expect("verify"));
    }

    #[test]
    fn hash_with_custom_cost_round_trips() {
        let password = "test";
        let hashed = hash_with_cost(password, 4).expect("hash");
        let bcrypt = BcryptHasher::new(BcryptOptions { rounds: 4 });
        assert!(verify_with(&bcrypt, password, &hashed).expect("verify"));
    }

    #[test]
    fn bcrypt_needs_rehash_on_low_cost() {
        let driver = bcrypt_default();
        let low_cost_hash = hash_with_cost("test", 4).expect("hash");
        assert!(needs_rehash_with(&driver, &low_cost_hash));
        let default_cost_hash = hash_with(&driver, "test").expect("hash");
        assert!(!needs_rehash_with(&driver, &default_cost_hash));
    }

    #[test]
    fn hash_with_cost_rejects_below_min() {
        let err = hash_with_cost("test", MIN_BCRYPT_COST - 1).expect_err("below-min must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("cost="),
            "msg should cite the offending cost: {msg}"
        );
        assert!(
            msg.contains("4..=31"),
            "msg should cite the valid range: {msg}"
        );
    }

    #[test]
    fn hash_with_cost_rejects_above_max() {
        let err = hash_with_cost("test", MAX_BCRYPT_COST + 1).expect_err("above-max must reject");
        let msg = format!("{err}");
        assert!(
            msg.contains("cost="),
            "msg should cite the offending cost: {msg}"
        );
        assert!(
            msg.contains("4..=31"),
            "msg should cite the valid range: {msg}"
        );
    }

    #[test]
    fn hash_with_cost_rejects_zero_and_far_above() {
        // Sanity: 0 and absurdly-high values both rejected.
        assert!(hash_with_cost("test", 0).is_err());
        assert!(hash_with_cost("test", u32::MAX).is_err());
    }

    #[test]
    fn hash_with_cost_accepts_endpoints() {
        // MIN endpoint must work — it's the fastest valid bcrypt cost.
        // (MAX endpoint isn't exercised because cost 31 takes hours.)
        assert!(hash_with_cost("test", MIN_BCRYPT_COST).is_ok());
    }

    #[tokio::test]
    async fn hash_with_cost_async_propagates_range_error() {
        let err = hash_with_cost_async("test", MAX_BCRYPT_COST + 1)
            .await
            .expect_err("above-max must reject");
        assert!(format!("{err}").contains("4..=31"));
    }
}
