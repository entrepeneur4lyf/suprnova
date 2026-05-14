//! Cache module for suprnova framework
//!
//! Provides a Redis-backed cache with automatic in-memory fallback.
//!
//! # Quick Start
//!
//! The cache is automatically initialized when the server starts. If Redis is
//! available (via `REDIS_URL`), it uses Redis. Otherwise, it falls back to
//! an in-memory cache.
//!
//! ```rust,ignore
//! use suprnova::Cache;
//! use std::time::Duration;
//!
//! // Store a value with 1 hour TTL
//! Cache::put("user:1", &user, Some(Duration::from_secs(3600))).await?;
//!
//! // Retrieve it
//! let cached: Option<User> = Cache::get("user:1").await?;
//!
//! // Check if exists
//! if Cache::has("user:1").await? {
//!     // ...
//! }
//!
//! // Remove it
//! Cache::forget("user:1").await?;
//!
//! // Clear all cache
//! Cache::flush().await?;
//! ```

pub mod config;
pub mod memory;
pub mod redis;
pub mod store;

pub use config::{CacheConfig, CacheConfigBuilder};
pub use memory::InMemoryCache;
pub use redis::RedisCache;
pub use store::CacheStore;

use crate::config::Config;
use crate::container::App;
use crate::error::FrameworkError;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Cache facade - main entry point for cache operations
///
/// Provides static methods for accessing the cache. The cache store
/// is automatically initialized when the server starts.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::Cache;
/// use std::time::Duration;
///
/// // Store with TTL
/// Cache::put("key", &value, Some(Duration::from_secs(3600))).await?;
///
/// // Store forever (no expiration)
/// Cache::forever("key", &value).await?;
///
/// // Retrieve
/// let value: Option<MyType> = Cache::get("key").await?;
///
/// // Get or compute (remember pattern)
/// let value = Cache::remember("key", Some(Duration::from_secs(3600)), || async {
///     expensive_computation().await
/// }).await?;
/// ```
pub struct Cache;

impl Cache {
    /// Bootstrap the cache system
    ///
    /// Tries to connect to Redis first. If Redis is unavailable,
    /// falls back to in-memory cache automatically.
    ///
    /// This is called automatically by `Server::run()`.
    pub(crate) async fn bootstrap() {
        let config = Config::get::<CacheConfig>().unwrap_or_default();

        // Try Redis first
        match RedisCache::connect(&config).await {
            Ok(redis_cache) => {
                App::bind::<dyn CacheStore>(Arc::new(redis_cache));
            }
            Err(_) => {
                // Fallback to in-memory
                let memory_cache = InMemoryCache::with_prefix(&config.prefix);
                App::bind::<dyn CacheStore>(Arc::new(memory_cache));
            }
        }
    }

    /// Get the underlying cache store
    pub fn store() -> Result<Arc<dyn CacheStore>, FrameworkError> {
        App::resolve_make::<dyn CacheStore>()
    }

    /// Check if the cache is initialized
    pub fn is_initialized() -> bool {
        App::has_binding::<dyn CacheStore>()
    }

    // =========================================================================
    // Main cache operations
    // =========================================================================

    /// Retrieve an item from the cache
    ///
    /// Returns `None` if the key doesn't exist or has expired.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let user: Option<User> = Cache::get("user:1").await?;
    /// ```
    pub async fn get<T: DeserializeOwned>(key: &str) -> Result<Option<T>, FrameworkError> {
        let store = Self::store()?;
        match store.get_raw(key).await? {
            Some(json) => {
                let value = serde_json::from_str(&json).map_err(|e| {
                    FrameworkError::internal(format!("Cache deserialize error: {}", e))
                })?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    /// Store an item in the cache
    ///
    /// If `ttl` is `None`, uses the default TTL from config (or no expiration if 0).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Cache::put("user:1", &user, Some(Duration::from_secs(3600))).await?;
    /// ```
    pub async fn put<T: Serialize>(
        key: &str,
        value: &T,
        ttl: Option<Duration>,
    ) -> Result<(), FrameworkError> {
        let store = Self::store()?;
        let json = serde_json::to_string(value).map_err(|e| {
            FrameworkError::internal(format!("Cache serialize error: {}", e))
        })?;
        store.put_raw(key, &json, ttl).await
    }

    /// Store an item forever (no expiration)
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Cache::forever("config:settings", &settings).await?;
    /// ```
    pub async fn forever<T: Serialize>(key: &str, value: &T) -> Result<(), FrameworkError> {
        Self::put(key, value, None).await
    }

    /// Check if a key exists in the cache
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// if Cache::has("user:1").await? {
    ///     println!("User is cached");
    /// }
    /// ```
    pub async fn has(key: &str) -> Result<bool, FrameworkError> {
        let store = Self::store()?;
        store.has(key).await
    }

    /// Remove an item from the cache
    ///
    /// Returns `true` if the item existed and was removed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Cache::forget("user:1").await?;
    /// ```
    pub async fn forget(key: &str) -> Result<bool, FrameworkError> {
        let store = Self::store()?;
        store.forget(key).await
    }

    /// Remove all items from the cache
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Cache::flush().await?;
    /// ```
    pub async fn flush() -> Result<(), FrameworkError> {
        let store = Self::store()?;
        store.flush().await
    }

    /// Increment a numeric value
    ///
    /// If the key doesn't exist, it's initialized to 0 before incrementing.
    /// Returns the new value.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let count = Cache::increment("visits", 1).await?;
    /// ```
    pub async fn increment(key: &str, amount: i64) -> Result<i64, FrameworkError> {
        let store = Self::store()?;
        store.increment(key, amount).await
    }

    /// Decrement a numeric value
    ///
    /// If the key doesn't exist, it's initialized to 0 before decrementing.
    /// Returns the new value.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let remaining = Cache::decrement("quota", 1).await?;
    /// ```
    pub async fn decrement(key: &str, amount: i64) -> Result<i64, FrameworkError> {
        let store = Self::store()?;
        store.decrement(key, amount).await
    }

    /// Get an item or store a default value if it doesn't exist
    ///
    /// If the key exists, returns the cached value.
    /// If not, calls the closure to compute the value, stores it, and returns it.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let user = Cache::remember("user:1", Some(Duration::from_secs(3600)), || async {
    ///     User::find(1).await
    /// }).await?;
    /// ```
    pub async fn remember<T, F, Fut>(
        key: &str,
        ttl: Option<Duration>,
        default: F,
    ) -> Result<T, FrameworkError>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, FrameworkError>>,
    {
        // Try to get from cache first
        if let Some(cached) = Self::get::<T>(key).await? {
            return Ok(cached);
        }

        // Compute the value
        let value = default().await?;

        // Store it
        Self::put(key, &value, ttl).await?;

        Ok(value)
    }

    /// Get an item or store a default value forever
    ///
    /// Same as `remember` but with no expiration.
    pub async fn remember_forever<T, F, Fut>(key: &str, default: F) -> Result<T, FrameworkError>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, FrameworkError>>,
    {
        Self::remember(key, None, default).await
    }
}
