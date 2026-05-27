//! Idempotency keys backed by cache locks.
//!
//! Three entry points, increasing in guarantee:
//!
//! - [`Idempotency::once`] — dedupe only. Runs `body` for the first caller in
//!   the TTL window; duplicates get [`Idempotent::Duplicate`] and the body does
//!   NOT run. The lock is never released — `ttl` IS the dedupe window.
//! - [`Idempotency::commit_on_success`] — like `once`, but releases the lock if
//!   `body` returns `Err`, so a transient failure can be retried within the
//!   window.
//! - [`Idempotency::remember`] — dedupe WITH result replay. Stores the success
//!   value and replays it to duplicate callers ([`Replay::Replayed`]). This is
//!   the Stripe-style idempotency-key model for HTTP endpoints and queue jobs
//!   that must return the original outcome, not merely skip re-execution.
//!
//! Caller-supplied key material is hashed before it touches the cache backend,
//! so arbitrary client-supplied keys cannot produce unbounded backend keys,
//! leak raw identifiers into cache tooling, or inject characters that collide
//! with backend key conventions.

use crate::cache::Cache;
use crate::error::FrameworkError;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::time::Duration;

/// The outcome of a dedupe-only idempotent operation
/// ([`Idempotency::once`] / [`Idempotency::commit_on_success`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Idempotent<T> {
    /// First caller in the TTL window — `body` ran and produced this value.
    Fresh(T),
    /// Duplicate caller within the TTL window — `body` was NOT run.
    Duplicate,
}

/// The outcome of a result-replaying idempotent operation
/// ([`Idempotency::remember`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Replay<T> {
    /// First caller — `body` ran, produced this value, and recorded it for replay.
    Fresh(T),
    /// Duplicate caller — `body` already completed; this is the recorded result.
    Replayed(T),
    /// Duplicate caller arriving while the original `body` is still running, with
    /// no recorded result yet. The original has not finished, so there is nothing
    /// to replay. HTTP callers typically map this to `409 Conflict` and retry.
    InProgress,
}

/// Thin wrapper over [`Cache::lock`] providing idempotency-key semantics.
pub struct Idempotency;

impl Idempotency {
    /// Run `body` exactly once per `key` within the given `ttl` window.
    ///
    /// - First caller: acquires a lock at `idem:<hash>`, runs `body`, returns
    ///   `Ok(Idempotent::Fresh(result))`.
    /// - Subsequent callers within `ttl`: return `Ok(Idempotent::Duplicate)`
    ///   without running `body`.
    ///
    /// The lock is intentionally NOT released on success — the TTL IS the
    /// dedupe window. Because the window only matters after the body completes,
    /// `body` running longer than `ttl` does not collapse the window: the lock
    /// is refreshed in the background for the body's duration (see the crate
    /// module docs on lease renewal).
    ///
    /// Choose [`commit_on_success`](Self::commit_on_success) instead when a
    /// failed `body` should be retryable within the window, or
    /// [`remember`](Self::remember) when duplicates must receive the original
    /// result rather than a bare `Duplicate` marker.
    ///
    /// # Errors
    ///
    /// Propagates any [`FrameworkError`] from the cache layer or from `body`.
    pub async fn once<F, Fut, T>(
        key: &str,
        ttl: Duration,
        body: F,
    ) -> Result<Idempotent<T>, FrameworkError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, FrameworkError>>,
    {
        let guard = Cache::lock(&lock_key(key), ttl).await?;
        match guard {
            Some(_g) => {
                let v = body().await?;
                Ok(Idempotent::Fresh(v))
            }
            None => Ok(Idempotent::Duplicate),
        }
    }

    /// Like [`once`](Self::once), but releases the dedupe lock if `body` returns
    /// `Err`, allowing the operation to be retried within the TTL window.
    ///
    /// Use this when:
    /// - The body has retryable failure modes (transient network error,
    ///   rate limit, etc.)
    /// - You want at-least-once semantics for success and at-most-once
    ///   only after success
    ///
    /// Use [`once`](Self::once) instead when:
    /// - The body's side effects must not repeat regardless of outcome
    ///   (e.g., "I sent the email; even if I errored after sending,
    ///   don't try again")
    ///
    /// # Errors
    ///
    /// Propagates any [`FrameworkError`] from the cache layer or from `body`.
    pub async fn commit_on_success<F, Fut, T>(
        key: &str,
        ttl: Duration,
        body: F,
    ) -> Result<Idempotent<T>, FrameworkError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, FrameworkError>>,
    {
        let guard = Cache::lock(&lock_key(key), ttl).await?;
        match guard {
            Some(g) => match body().await {
                Ok(v) => Ok(Idempotent::Fresh(v)),
                Err(e) => {
                    // Release the lock so a retry within the window can re-enter.
                    let _ = g.release().await;
                    Err(e)
                }
            },
            None => Ok(Idempotent::Duplicate),
        }
    }

    /// Run `body` once per `key` and replay its recorded result to duplicate
    /// callers within the TTL window.
    ///
    /// Unlike [`once`](Self::once) (which only tells a duplicate that the work
    /// already happened), `remember` records the success value and hands it back
    /// to later callers — the Stripe-style idempotency-key contract that lets an
    /// HTTP endpoint or queue job return the original response on retry.
    ///
    /// Outcomes:
    /// - [`Replay::Fresh`] — first caller; `body` ran and the result was recorded.
    /// - [`Replay::Replayed`] — duplicate; the recorded result is returned and
    ///   `body` did NOT run.
    /// - [`Replay::InProgress`] — duplicate arriving while the original `body` is
    ///   still running; there is no recorded result yet. Map this to a retryable
    ///   `409 Conflict` at the HTTP layer.
    ///
    /// Semantics:
    /// - **Errors do not replay.** A failing `body` releases the lock and returns
    ///   the error, so the operation is retryable within the window (same policy
    ///   as [`commit_on_success`](Self::commit_on_success)). Only success values
    ///   are recorded.
    /// - **The result payload reaches the cache backend.** The *key* is hashed,
    ///   but `T` is serialized verbatim; do not place secrets in a replayed value
    ///   that must not appear in your cache store.
    /// - **`body` should be cancel-safe** if the caller may drop this future:
    ///   cancellation drops `body` mid-await like any other tokio future.
    ///
    /// # Errors
    ///
    /// Propagates any [`FrameworkError`] from the cache layer or from `body`.
    pub async fn remember<F, Fut, T>(
        key: &str,
        ttl: Duration,
        body: F,
    ) -> Result<Replay<T>, FrameworkError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, FrameworkError>>,
        T: Serialize + DeserializeOwned,
    {
        let h = hashed(key);
        let lock_key = format!("idem:{h}");
        let result_key = format!("idem:{h}:result");

        // 1. Fast path: a result is already recorded — replay it without locking.
        if let Some(value) = Cache::get::<T>(&result_key).await? {
            return Ok(Replay::Replayed(value));
        }

        // 2. Try to claim the lock so we are the only caller running `body`.
        match Cache::lock(&lock_key, ttl).await? {
            Some(guard) => {
                // 2a. Re-check after acquiring: a result may have been stored
                //     between the step-1 read and the lock acquisition.
                if let Some(value) = Cache::get::<T>(&result_key).await? {
                    let _ = guard.release().await;
                    return Ok(Replay::Replayed(value));
                }
                // 2b. Run the body, then record the result BEFORE releasing the
                //     lock so no duplicate can slip in and re-run between the
                //     store and the release. If the store fails, the guard drops
                //     un-released and the lock holds until TTL (fail-closed:
                //     duplicates see InProgress, never a second execution).
                match body().await {
                    Ok(value) => {
                        Cache::put(&result_key, &value, Some(ttl)).await?;
                        let _ = guard.release().await;
                        Ok(Replay::Fresh(value))
                    }
                    Err(e) => {
                        let _ = guard.release().await;
                        Err(e)
                    }
                }
            }
            // 3. Contended: another caller holds the lock and is running `body`.
            //    Re-read once in case they finished between the attempt and now.
            None => match Cache::get::<T>(&result_key).await? {
                Some(value) => Ok(Replay::Replayed(value)),
                None => Ok(Replay::InProgress),
            },
        }
    }
}

/// Derive the lock-key argument for [`Cache::lock`] from caller-supplied key
/// material. The raw key is hashed (see [`hashed`]) so arbitrary client input
/// cannot bloat or pollute backend keys.
fn lock_key(key: &str) -> String {
    format!("idem:{}", hashed(key))
}

/// Hash caller-supplied key material into a fixed-length hex digest.
///
/// Bounds backend key length to 64 hex chars regardless of input, keeps raw
/// identifiers (which may be PII or client-controlled) out of cache tooling,
/// and strips any characters that could collide with backend key conventions.
fn hashed(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(key.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed_is_fixed_length_hex_regardless_of_input() {
        let short = hashed("k");
        let long = hashed(&"x".repeat(100_000));
        assert_eq!(short.len(), 64);
        assert_eq!(long.len(), 64);
        assert!(short.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(long.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hashed_does_not_leak_raw_key_material() {
        let raw = "user-4242-card-secret";
        let key = lock_key(raw);
        assert!(!key.contains(raw), "raw key material leaked into cache key");
        assert!(key.starts_with("idem:"));
    }

    #[test]
    fn distinct_keys_hash_distinctly() {
        assert_ne!(hashed("a"), hashed("b"));
    }
}
