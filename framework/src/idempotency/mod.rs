//! Idempotency keys backed by cache locks.
//!
//! `Idempotency::once(key, ttl, body)` claims `idem:<key>` for `ttl`
//! and runs `body` only on the first caller within the window.
//! Duplicate callers return `Idempotent::Duplicate` without running
//! the body. The lock is NOT released — `ttl` IS the dedupe window.

use crate::cache::Cache;
use crate::error::FrameworkError;
use std::future::Future;
use std::time::Duration;

/// The outcome of an idempotent operation.
#[derive(Debug, PartialEq, Eq)]
pub enum Idempotent<T> {
    /// First caller in the TTL window — `body` ran and produced this value.
    Fresh(T),
    /// Duplicate caller within the TTL window — `body` was NOT run.
    Duplicate,
}

/// Thin wrapper over [`Cache::lock`] providing idempotency-key semantics.
pub struct Idempotency;

impl Idempotency {
    /// Run `body` exactly once per `key` within the given `ttl` window.
    ///
    /// - First caller: acquires a lock at `idem:<key>`, runs `body`, returns
    ///   `Ok(Idempotent::Fresh(result))`.
    /// - Subsequent callers within `ttl`: return `Ok(Idempotent::Duplicate)`
    ///   without running `body`.
    ///
    /// The lock is intentionally NOT released on success — the TTL IS the
    /// dedupe window.
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
        // Cache::lock acquires `lock:<key>` inside the cache's prefix namespace.
        // We do NOT call .release() — the TTL is the dedupe window.
        let guard = Cache::lock(&format!("idem:{key}"), ttl).await?;
        match guard {
            Some(_g) => {
                let v = body().await?;
                Ok(Idempotent::Fresh(v))
            }
            None => Ok(Idempotent::Duplicate),
        }
    }
}
