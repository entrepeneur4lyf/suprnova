//! In-memory cache implementation for testing and fallback
//!
//! Provides a thread-safe in-memory cache that mimics Redis behavior.
//! Supports TTL expiration.

use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use super::config::CacheConfig;
use super::store::CacheStore;
use crate::error::FrameworkError;

/// In-memory cache entry with optional expiration and current tag set.
///
/// `tags` is the per-entry source of truth: a tagged write records the
/// new tag set on the entry, and `flush_tags` consults it before
/// deleting. That makes overwriting a tagged key with an untagged
/// `put_raw` safe — the entry's tag set is cleared and a later
/// `flush_tags(t)` will not touch the live untagged value.
#[derive(Clone)]
struct CacheEntry {
    value: String,
    expires_at: Option<Instant>,
    tags: HashSet<String>,
}

impl CacheEntry {
    fn is_expired(&self) -> bool {
        self.expires_at.map(|t| Instant::now() > t).unwrap_or(false)
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
    /// Tag -> candidate keys index. Used as a fast lookup by
    /// `flush_tags`; the entry's own `tags` set is the source of truth.
    /// Stale entries in here are harmless because the per-entry check
    /// rejects them, and they get pruned during the flush walk.
    tag_index: RwLock<HashMap<String, HashSet<String>>>,
    prefix: String,
    /// Default TTL applied by the `Cache` facade when callers pass `None`
    /// to `put` / `tags_put`. `Cache::forever` and direct `put_raw(None)`
    /// calls bypass this. `None` means no facade-level default.
    default_ttl: Option<Duration>,
}

impl InMemoryCache {
    /// Create a new empty in-memory cache
    pub fn new() -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
            tag_index: RwLock::new(HashMap::new()),
            prefix: "suprnova_cache:".to_string(),
            default_ttl: None,
        }
    }

    /// Create with a custom prefix
    pub fn with_prefix(prefix: impl Into<String>) -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
            tag_index: RwLock::new(HashMap::new()),
            prefix: prefix.into(),
            default_ttl: None,
        }
    }

    /// Create from a `CacheConfig` — picks up both the prefix and the
    /// configured `default_ttl` so that the facade-level default TTL
    /// applies uniformly across in-memory and Redis backends.
    pub fn with_config(config: &CacheConfig) -> Self {
        let default_ttl = if config.default_ttl > 0 {
            Some(Duration::from_secs(config.default_ttl))
        } else {
            None
        };
        Self {
            store: RwLock::new(HashMap::new()),
            tag_index: RwLock::new(HashMap::new()),
            prefix: config.prefix.clone(),
            default_ttl,
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
    fn default_ttl(&self) -> Option<Duration> {
        self.default_ttl
    }

    async fn get_raw(&self, key: &str) -> Result<Option<String>, FrameworkError> {
        let key = self.prefixed_key(key);

        let store = self
            .store
            .read()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;

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

        // Untagged write: overwrites the entry with an empty tag set
        // and proactively prunes the forward `tag_index` of any old
        // references to this key. The validation gate in `flush_tags`
        // would catch a stale reference anyway, but pruning keeps the
        // index from growing indefinitely for tags that are written
        // but never flushed.
        let entry = CacheEntry {
            value: value.to_string(),
            expires_at: ttl.map(|d| Instant::now() + d),
            tags: HashSet::new(),
        };

        let mut store = self
            .store
            .write()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;
        let mut idx = self
            .tag_index
            .write()
            .map_err(|_| FrameworkError::internal("Tag index poisoned"))?;

        let old_tags: Vec<String> = store
            .get(&key)
            .map(|e| e.tags.iter().cloned().collect())
            .unwrap_or_default();
        for t in &old_tags {
            if let Some(set) = idx.get_mut(t) {
                set.remove(&key);
                if set.is_empty() {
                    idx.remove(t);
                }
            }
        }

        store.insert(key, entry);
        Ok(())
    }

    async fn add_raw(
        &self,
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<bool, FrameworkError> {
        let pkey = self.prefixed_key(key);

        let mut store = self
            .store
            .write()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;
        let mut idx = self
            .tag_index
            .write()
            .map_err(|_| FrameworkError::internal("Tag index poisoned"))?;

        // Atomic: hold the write lock across the existence check and the
        // insert so two concurrent `add_raw` calls cannot both succeed.
        let occupied = store.get(&pkey).map(|e| !e.is_expired()).unwrap_or(false);
        if occupied {
            return Ok(false);
        }

        // Untagged write: prune any leftover tag_index references for an
        // expired or absent prior entry so the index can't accumulate
        // dead pointers from a previously-tagged write.
        let stale_tags: Vec<String> = store
            .get(&pkey)
            .map(|e| e.tags.iter().cloned().collect())
            .unwrap_or_default();
        for t in &stale_tags {
            if let Some(set) = idx.get_mut(t) {
                set.remove(&pkey);
                if set.is_empty() {
                    idx.remove(t);
                }
            }
        }

        store.insert(
            pkey,
            CacheEntry {
                value: value.to_string(),
                expires_at: ttl.map(|d| Instant::now() + d),
                tags: HashSet::new(),
            },
        );
        Ok(true)
    }

    async fn has(&self, key: &str) -> Result<bool, FrameworkError> {
        let key = self.prefixed_key(key);

        let store = self
            .store
            .read()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;

        Ok(store.get(&key).map(|e| !e.is_expired()).unwrap_or(false))
    }

    async fn forget(&self, key: &str) -> Result<bool, FrameworkError> {
        let key = self.prefixed_key(key);

        let mut store = self
            .store
            .write()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;
        let mut idx = self
            .tag_index
            .write()
            .map_err(|_| FrameworkError::internal("Tag index poisoned"))?;

        // Prune forward index references so the tag_index does not
        // accumulate dangling pointers to forgotten keys.
        let removed_tags: Vec<String> = store
            .get(&key)
            .map(|e| e.tags.iter().cloned().collect())
            .unwrap_or_default();
        let existed = store.remove(&key).is_some();
        for t in &removed_tags {
            if let Some(set) = idx.get_mut(t) {
                set.remove(&key);
                if set.is_empty() {
                    idx.remove(t);
                }
            }
        }
        Ok(existed)
    }

    async fn flush(&self) -> Result<(), FrameworkError> {
        let mut store = self
            .store
            .write()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;
        let mut idx = self
            .tag_index
            .write()
            .map_err(|_| FrameworkError::internal("Tag index poisoned"))?;

        // Clear both the value store and the tag index — leaving stale
        // tag candidates pointing at deleted keys would let a later
        // `flush_tags` walk a long-dead forward index.
        store.clear();
        idx.clear();
        Ok(())
    }

    async fn increment(&self, key: &str, amount: i64) -> Result<i64, FrameworkError> {
        let key = self.prefixed_key(key);

        let mut store = self
            .store
            .write()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;

        // Preserve the existing entry's TTL on increment — matches Redis
        // `INCR` semantics, which never resets the key's expiration. The
        // rate-limit fixed-window counter relies on this: the counter
        // shares its TTL with the `:timer` deadline so both ages out
        // together when the window ends.
        let (current, expires_at, tags): (i64, Option<Instant>, HashSet<String>) = store
            .get(&key)
            .filter(|e| !e.is_expired())
            .map(|e| (e.value.parse().unwrap_or(0), e.expires_at, e.tags.clone()))
            .unwrap_or((0, None, HashSet::new()));

        let new_value = current + amount;

        store.insert(
            key,
            CacheEntry {
                value: new_value.to_string(),
                expires_at,
                tags,
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
        let tag_set: HashSet<String> = tags.iter().map(|t| (*t).to_string()).collect();

        let mut s = self
            .store
            .write()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;
        let mut idx = self
            .tag_index
            .write()
            .map_err(|_| FrameworkError::internal("Tag index poisoned"))?;

        // Prune forward index references for tags this key WAS in but is
        // not in anymore (so re-tagging `["a"] -> ["b"]` doesn't leave
        // `a -> {key}` pointing here). The validation gate handles
        // correctness on flush; this just keeps the index from growing.
        let stale_tags: Vec<String> = s
            .get(&pkey)
            .map(|e| e.tags.difference(&tag_set).cloned().collect())
            .unwrap_or_default();
        for t in &stale_tags {
            if let Some(set) = idx.get_mut(t) {
                set.remove(&pkey);
                if set.is_empty() {
                    idx.remove(t);
                }
            }
        }

        // Overwrite installs the new tag set on the entry — replaces
        // (not unions with) any prior tags. This is what makes a
        // tagged overwrite drop old tag memberships from the source
        // of truth.
        s.insert(
            pkey.clone(),
            CacheEntry {
                value: value.into(),
                expires_at: ttl.map(|d| Instant::now() + d),
                tags: tag_set,
            },
        );
        for t in tags {
            idx.entry((*t).into()).or_default().insert(pkey.clone());
        }
        Ok(())
    }

    async fn flush_tags(&self, tags: &[&str]) -> Result<(), FrameworkError> {
        // Pull the candidate key lists out first so we can hold the
        // value-store write lock for the validation pass without keeping
        // both locks at once.
        let candidates: Vec<(String, Vec<String>)> = {
            let mut idx = self
                .tag_index
                .write()
                .map_err(|_| FrameworkError::internal("Tag index poisoned"))?;
            tags.iter()
                .map(|t| {
                    let members = idx
                        .remove(*t)
                        .map(|set| set.into_iter().collect::<Vec<_>>())
                        .unwrap_or_default();
                    ((*t).to_string(), members)
                })
                .collect()
        };

        let mut s = self
            .store
            .write()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;
        for (tag, members) in candidates {
            for k in members {
                // Validate against the entry's own tag set before
                // deleting. If the entry was overwritten untagged
                // (tags.is_empty()) or never had this tag (re-tagged to
                // something else), leave it alone — only the now-stale
                // forward index entry pointed here.
                let should_delete = match s.get(&k) {
                    Some(e) if !e.is_expired() => e.tags.contains(&tag),
                    // Expired or missing values are not deletions the
                    // caller needs to see; the value is already gone.
                    _ => false,
                };
                if should_delete {
                    s.remove(&k);
                }
            }
        }
        Ok(())
    }

    async fn acquire_lock(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<Option<String>, FrameworkError> {
        let pkey = self.prefixed_key(&format!("lock:{key}"));
        let mut s = self
            .store
            .write()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;
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
                tags: HashSet::new(),
            },
        );
        Ok(Some(token))
    }

    async fn release_lock(&self, key: &str, token: &str) -> Result<bool, FrameworkError> {
        let pkey = self.prefixed_key(&format!("lock:{key}"));
        let mut s = self
            .store
            .write()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;
        match s.get(&pkey) {
            Some(e) if !e.is_expired() && e.value == token => {
                s.remove(&pkey);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn refresh_lock(
        &self,
        key: &str,
        token: &str,
        ttl: Duration,
    ) -> Result<bool, FrameworkError> {
        let pkey = self.prefixed_key(&format!("lock:{key}"));
        let mut s = self
            .store
            .write()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;
        match s.get_mut(&pkey) {
            Some(e) if !e.is_expired() && e.value == token => {
                e.expires_at = Some(Instant::now() + ttl);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn touch(&self, key: &str, ttl: Duration) -> Result<bool, FrameworkError> {
        let pkey = self.prefixed_key(key);
        let mut s = self
            .store
            .write()
            .map_err(|_| FrameworkError::internal("Cache lock poisoned"))?;
        match s.get_mut(&pkey) {
            Some(e) if !e.is_expired() => {
                e.expires_at = Some(Instant::now() + ttl);
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}
