//! In-memory cache implementation for testing and fallback
//!
//! Provides a thread-safe in-memory cache that mimics Redis behavior.
//! Supports TTL expiration.

use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use super::store::CacheStore;
use crate::error::FrameworkError;

/// In-memory cache entry with optional expiration
#[derive(Clone)]
struct CacheEntry {
    value: String,
    expires_at: Option<Instant>,
}

impl CacheEntry {
    fn is_expired(&self) -> bool {
        self.expires_at
            .map(|t| Instant::now() > t)
            .unwrap_or(false)
    }
}

/// In-memory cache implementation
///
/// Thread-safe cache that stores values in memory with optional TTL.
/// Use this as a fallback when Redis is unavailable, or in tests.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::cache::InMemoryCache;
///
/// let cache = InMemoryCache::new();
/// ```
pub struct InMemoryCache {
    store: RwLock<HashMap<String, CacheEntry>>,
    tag_index: RwLock<HashMap<String, HashSet<String>>>,
    prefix: String,
}

impl InMemoryCache {
    /// Create a new empty in-memory cache
    pub fn new() -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
            tag_index: RwLock::new(HashMap::new()),
            prefix: "suprnova_cache:".to_string(),
        }
    }

    /// Create with a custom prefix
    pub fn with_prefix(prefix: impl Into<String>) -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
            tag_index: RwLock::new(HashMap::new()),
            prefix: prefix.into(),
        }
    }

    fn prefixed_key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }
}

impl Default for InMemoryCache {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CacheStore for InMemoryCache {
    async fn get_raw(&self, key: &str) -> Result<Option<String>, FrameworkError> {
        let key = self.prefixed_key(key);

        let store = self.store.read().map_err(|_| {
            FrameworkError::internal("Cache lock poisoned")
        })?;

        match store.get(&key) {
            Some(entry) if !entry.is_expired() => Ok(Some(entry.value.clone())),
            _ => Ok(None),
        }
    }

    async fn put_raw(
        &self,
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<(), FrameworkError> {
        let key = self.prefixed_key(key);

        let entry = CacheEntry {
            value: value.to_string(),
            expires_at: ttl.map(|d| Instant::now() + d),
        };

        let mut store = self.store.write().map_err(|_| {
            FrameworkError::internal("Cache lock poisoned")
        })?;

        store.insert(key, entry);
        Ok(())
    }

    async fn has(&self, key: &str) -> Result<bool, FrameworkError> {
        let key = self.prefixed_key(key);

        let store = self.store.read().map_err(|_| {
            FrameworkError::internal("Cache lock poisoned")
        })?;

        Ok(store.get(&key).map(|e| !e.is_expired()).unwrap_or(false))
    }

    async fn forget(&self, key: &str) -> Result<bool, FrameworkError> {
        let key = self.prefixed_key(key);

        let mut store = self.store.write().map_err(|_| {
            FrameworkError::internal("Cache lock poisoned")
        })?;

        Ok(store.remove(&key).is_some())
    }

    async fn flush(&self) -> Result<(), FrameworkError> {
        let mut store = self.store.write().map_err(|_| {
            FrameworkError::internal("Cache lock poisoned")
        })?;

        store.clear();
        Ok(())
    }

    async fn increment(&self, key: &str, amount: i64) -> Result<i64, FrameworkError> {
        let key = self.prefixed_key(key);

        let mut store = self.store.write().map_err(|_| {
            FrameworkError::internal("Cache lock poisoned")
        })?;

        let current: i64 = store
            .get(&key)
            .filter(|e| !e.is_expired())
            .and_then(|e| e.value.parse().ok())
            .unwrap_or(0);

        let new_value = current + amount;

        store.insert(
            key,
            CacheEntry {
                value: new_value.to_string(),
                expires_at: None,
            },
        );

        Ok(new_value)
    }

    async fn decrement(&self, key: &str, amount: i64) -> Result<i64, FrameworkError> {
        self.increment(key, -amount).await
    }

    async fn tagged_put_raw(
        &self,
        tags: &[&str],
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<(), FrameworkError> {
        let pkey = self.prefixed_key(key);
        {
            let mut s = self.store.write().map_err(|_| {
                FrameworkError::internal("Cache lock poisoned")
            })?;
            s.insert(
                pkey.clone(),
                CacheEntry {
                    value: value.into(),
                    expires_at: ttl.map(|d| Instant::now() + d),
                },
            );
        }
        let mut idx = self.tag_index.write().map_err(|_| {
            FrameworkError::internal("Tag index poisoned")
        })?;
        for t in tags {
            idx.entry((*t).into()).or_default().insert(pkey.clone());
        }
        Ok(())
    }

    async fn flush_tags(&self, tags: &[&str]) -> Result<(), FrameworkError> {
        let keys: Vec<String> = {
            let mut idx = self.tag_index.write().map_err(|_| {
                FrameworkError::internal("Tag index poisoned")
            })?;
            let mut out = Vec::new();
            for t in tags {
                if let Some(set) = idx.remove(*t) {
                    out.extend(set);
                }
            }
            out
        };
        let mut s = self.store.write().map_err(|_| {
            FrameworkError::internal("Cache lock poisoned")
        })?;
        for k in keys {
            s.remove(&k);
        }
        Ok(())
    }

    async fn acquire_lock(&self, key: &str, ttl: Duration) -> Result<Option<String>, FrameworkError> {
        let pkey = self.prefixed_key(&format!("lock:{key}"));
        let mut s = self.store.write().map_err(|_| {
            FrameworkError::internal("Cache lock poisoned")
        })?;
        let still_valid = s.get(&pkey).map(|e| !e.is_expired()).unwrap_or(false);
        if still_valid {
            return Ok(None);
        }
        let token = uuid::Uuid::new_v4().to_string();
        s.insert(
            pkey,
            CacheEntry {
                value: token.clone(),
                expires_at: Some(Instant::now() + ttl),
            },
        );
        Ok(Some(token))
    }

    async fn release_lock(&self, key: &str, token: &str) -> Result<bool, FrameworkError> {
        let pkey = self.prefixed_key(&format!("lock:{key}"));
        let mut s = self.store.write().map_err(|_| {
            FrameworkError::internal("Cache lock poisoned")
        })?;
        match s.get(&pkey) {
            Some(e) if !e.is_expired() && e.value == token => {
                s.remove(&pkey);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn refresh_lock(&self, key: &str, token: &str, ttl: Duration) -> Result<bool, FrameworkError> {
        let pkey = self.prefixed_key(&format!("lock:{key}"));
        let mut s = self.store.write().map_err(|_| {
            FrameworkError::internal("Cache lock poisoned")
        })?;
        match s.get_mut(&pkey) {
            Some(e) if !e.is_expired() && e.value == token => {
                e.expires_at = Some(Instant::now() + ttl);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
