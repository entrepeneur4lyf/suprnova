//! Cache store trait definition
//!
//! Defines the contract for cache implementations (Redis, InMemory, etc.)

use async_trait::async_trait;
use std::time::Duration;

use crate::error::FrameworkError;

/// Cache store trait - all cache backends must implement this
///
/// This trait uses JSON strings for values to enable dynamic typing.
/// The `Cache` facade handles serialization/deserialization.
///
/// # TTL contract
///
/// All write methods that take an `Option<Duration>` interpret `None`
/// as **no expiration** (literal forever). Defaulting `None` to a
/// configured TTL is the facade's responsibility, not the store's —
/// otherwise `Cache::forever` would not be forever on backends that
/// substitute a default. See `Cache::forever` for the call path that
/// bypasses any facade-level default.
#[async_trait]
pub trait CacheStore: Send + Sync {
    /// Retrieve a raw JSON value from the cache by key
    async fn get_raw(&self, key: &str) -> Result<Option<String>, FrameworkError>;

    /// Store a raw JSON value in the cache.
    ///
    /// `None` ttl means **no expiration**. Callers that want to apply a
    /// configured default TTL must resolve it before calling this method.
    async fn put_raw(
        &self,
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<(), FrameworkError>;

    /// The default TTL configured for `Cache::put` / `Cache::tags_put`
    /// when callers pass `None`. The facade reads this to resolve its
    /// own default; the store itself never substitutes it. `None` means
    /// no configured default (writes with `None` truly never expire).
    fn default_ttl(&self) -> Option<Duration> {
        None
    }

    /// Check if a key exists in the cache
    async fn has(&self, key: &str) -> Result<bool, FrameworkError>;

    /// Remove an item from the cache
    async fn forget(&self, key: &str) -> Result<bool, FrameworkError>;

    /// Remove all items from the cache
    async fn flush(&self) -> Result<(), FrameworkError>;

    /// Increment a numeric value
    ///
    /// Returns the new value after incrementing.
    async fn increment(&self, key: &str, amount: i64) -> Result<i64, FrameworkError>;

    /// Decrement a numeric value
    ///
    /// Returns the new value after decrementing.
    async fn decrement(&self, key: &str, amount: i64) -> Result<i64, FrameworkError>;

    /// Store a tagged value. The tag index is updated on every write —
    /// flushing a tag deletes every key associated with that tag.
    ///
    /// `None` ttl means **no expiration**; the facade resolves any
    /// configured default before calling this method.
    async fn tagged_put_raw(
        &self,
        tags: &[&str],
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<(), FrameworkError>;

    /// Remove every key associated with any of the given tags. Untagged
    /// keys with the same name are unaffected unless they were
    /// subsequently overwritten with a tagged write.
    async fn flush_tags(&self, tags: &[&str]) -> Result<(), FrameworkError>;

    /// Try to acquire a distributed lock for `key` with a TTL. On success,
    /// returns `Ok(Some(token))` where `token` is the ownership token needed
    /// for `release_lock` / `refresh_lock`. On contention, returns `Ok(None)`.
    async fn acquire_lock(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<Option<String>, FrameworkError>;

    /// Release a lock only if the supplied token matches the stored owner.
    /// Returns `true` if the lock was released; `false` if it was held by
    /// someone else or had already expired.
    async fn release_lock(&self, key: &str, token: &str) -> Result<bool, FrameworkError>;

    /// Extend the TTL of a lock only if the supplied token matches the stored
    /// owner. Returns `true` if the TTL was extended; `false` otherwise.
    async fn refresh_lock(
        &self,
        key: &str,
        token: &str,
        ttl: Duration,
    ) -> Result<bool, FrameworkError>;

    /// Refresh the TTL of an existing key without changing its value.
    /// Returns `true` if the key existed (and wasn't expired) and was
    /// refreshed; `false` otherwise.
    async fn touch(&self, key: &str, ttl: Duration) -> Result<bool, FrameworkError>;
}
