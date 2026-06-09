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
//! ## Lease renewal
//!
//! All three keep the lock's lease alive for the duration of `body`: a body
//! that runs longer than `ttl` cannot let the lock expire and a second caller
//! execute concurrently. See `run_under_lease`.
//!
//! ## Key material
//!
//! Caller-supplied key material is hashed before it touches the cache backend,
//! so arbitrary client-supplied keys cannot produce unbounded backend keys,
//! leak raw identifiers into cache tooling, or inject characters that collide
//! with backend key conventions.
//!
//! ## Shared backend
//!
//! Cross-process dedupe requires a cross-process cache (e.g. Redis). With the
//! in-memory backend — or a Redis bootstrap that fell back to memory — the
//! dedupe window is per-process only.

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
    /// dedupe window. The lock's lease is refreshed in the background for the
    /// body's duration (see `run_under_lease`), so a body that runs longer
    /// than `ttl` does not collapse the window or allow concurrent execution;
    /// the effective window is "body duration + up to `ttl` after the last
    /// refresh".
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
        let h = hashed(key);
        let guard = Cache::lock(&format!("idem:{h}"), ttl).await?;
        match guard {
            Some(g) => {
                let v = run_under_lease(&g, ttl, &h, body()).await?;
                // Do NOT release — the TTL is the dedupe window.
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
        let h = hashed(key);
        let guard = Cache::lock(&format!("idem:{h}"), ttl).await?;
        match guard {
            Some(g) => match run_under_lease(&g, ttl, &h, body()).await {
                Ok(v) => Ok(Idempotent::Fresh(v)),
                Err(e) => {
                    // Release the lock so a retry within the window can re-enter.
                    release_and_log(g, &h).await;
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
    /// # Keyspace responsibility (cross-endpoint isolation)
    ///
    /// `remember` keys the lock + result cache on `hashed(key)` only —
    /// the caller's `key` is the entire isolation surface. The caller
    /// **MUST** namespace the key with the route + user / business
    /// identity before passing it in; otherwise an attacker who
    /// captures an idempotency key issued for endpoint A can present
    /// the same key to endpoint B and receive A's recorded `T` as a
    /// `Replay::Replayed(...)` (the function happily replays the
    /// cached value regardless of which endpoint asks for it).
    ///
    /// The recommended shape:
    ///
    /// ```rust,ignore
    /// // GOOD — endpoint + user namespace isolates the cache cell.
    /// let cache_key = format!(
    ///     "{}:{}:{}",
    ///     request.method(),
    ///     request.path(),
    ///     idempotency_key_from_client,
    /// );
    /// Idempotency::remember(&cache_key, ttl, body).await?
    ///
    /// // BAD — bare client key leaks across endpoints.
    /// Idempotency::remember(idempotency_key_from_client, ttl, body).await?
    /// ```
    ///
    /// Mirrors Stripe's published contract: an idempotency key is
    /// scoped to a single operation, and the server is expected to
    /// detect mismatched request fingerprints. Suprnova provides the
    /// replay primitive; you provide the scope.
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
                    release_and_log(guard, &h).await;
                    return Ok(Replay::Replayed(value));
                }
                // 2b. Run the body (lease kept alive throughout), then record the
                //     result BEFORE releasing the lock so no duplicate can slip in
                //     and re-run between the store and the release. If the store
                //     fails, the guard drops un-released and the lock holds until
                //     TTL (fail-closed: duplicates see InProgress, never a second
                //     execution).
                match run_under_lease(&guard, ttl, &h, body()).await {
                    Ok(value) => {
                        Cache::put(&result_key, &value, Some(ttl)).await?;
                        release_and_log(guard, &h).await;
                        Ok(Replay::Fresh(value))
                    }
                    Err(e) => {
                        release_and_log(guard, &h).await;
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

/// Run `body` while keeping the idempotency lock's lease alive.
///
/// A background task refreshes the lock at one-third of `ttl` (floored at 50ms
/// to avoid a busy-loop on pathologically small TTLs) for as long as `body` is
/// running, so a body that outlives its original `ttl` cannot let the lock
/// expire and a second caller execute concurrently — the double-execution
/// window the bare lock left open. The renewal task parks (never resolves) so
/// the `select!` always completes via `body`; if a refresh ever fails (token
/// lost or backend error) it logs once and stops renewing rather than spamming.
/// Tested with `ttl >= 1s`; a very short `ttl` may not refresh before the first
/// expiry.
async fn run_under_lease<T>(
    guard: &crate::cache::LockGuard,
    ttl: Duration,
    hashed_key: &str,
    body: impl Future<Output = Result<T, FrameworkError>>,
) -> Result<T, FrameworkError> {
    let renew = async {
        let interval = (ttl / 3).max(Duration::from_millis(50));
        loop {
            tokio::time::sleep(interval).await;
            match guard.refresh(ttl).await {
                Ok(true) => continue,
                Ok(false) => {
                    tracing::warn!(
                        idempotency_key = %hashed_key,
                        "idempotency lease lost (lock token no longer matches); not renewing"
                    );
                    break;
                }
                Err(e) => {
                    tracing::warn!(
                        idempotency_key = %hashed_key,
                        error = %e,
                        "idempotency lease refresh failed; not renewing"
                    );
                    break;
                }
            }
        }
        // Park forever so `select!` only ever completes through the body branch.
        std::future::pending::<()>().await;
    };

    tokio::pin!(body);
    tokio::select! {
        biased;
        result = &mut body => result,
        _ = renew => unreachable!("renewal future parks after the loop and never resolves"),
    }
}

/// Release the lock, logging (never returning) on failure.
///
/// A failed release does not change the caller's primary result, but a token
/// mismatch or backend error means a retry may stay blocked until the TTL
/// lapses, so it must be observable. Logs the hashed key, never the raw key
/// material.
async fn release_and_log(guard: crate::cache::LockGuard, hashed_key: &str) {
    match guard.release().await {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            idempotency_key = %hashed_key,
            "idempotency lock release found a token mismatch (already expired or taken over)"
        ),
        Err(e) => tracing::warn!(
            idempotency_key = %hashed_key,
            error = %e,
            "idempotency lock release failed"
        ),
    }
}

/// Hash caller-supplied key material into a fixed-length hex digest.
///
/// Bounds backend key length to 64 hex chars regardless of input, keeps raw
/// identifiers (which may be PII or client-controlled) out of cache tooling,
/// and strips any characters that could collide with backend key conventions.
fn hashed(key: &str) -> String {
    crate::hashing::sha256_hex(key)
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
        let key = format!("idem:{}", hashed(raw));
        assert!(!key.contains(raw), "raw key material leaked into cache key");
        assert!(key.starts_with("idem:"));
    }

    #[test]
    fn distinct_keys_hash_distinctly() {
        assert_ne!(hashed("a"), hashed("b"));
    }
}
