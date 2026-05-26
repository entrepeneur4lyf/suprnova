//! Redis-backed cache implementation

use async_trait::async_trait;
use redis::{AsyncCommands, Client, aio::ConnectionManager};
use std::time::Duration;

use super::config::CacheConfig;
use super::store::CacheStore;
use crate::error::FrameworkError;

/// Redis cache implementation
///
/// Uses redis-rs with async/tokio runtime for high-performance caching.
pub struct RedisCache {
    conn: ConnectionManager,
    prefix: String,
    default_ttl: Option<Duration>,
}

impl RedisCache {
    /// Create a new Redis cache connection
    pub async fn connect(config: &CacheConfig) -> Result<Self, FrameworkError> {
        let client = Client::open(config.url.as_str())
            .map_err(|e| FrameworkError::internal(format!("Redis connection error: {}", e)))?;

        let conn = ConnectionManager::new(client).await.map_err(|e| {
            FrameworkError::internal(format!("Redis connection manager error: {}", e))
        })?;

        let default_ttl = if config.default_ttl > 0 {
            Some(Duration::from_secs(config.default_ttl))
        } else {
            None
        };

        Ok(Self {
            conn,
            prefix: config.prefix.clone(),
            default_ttl,
        })
    }

    fn prefixed_key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }
}

#[async_trait]
impl CacheStore for RedisCache {
    async fn get_raw(&self, key: &str) -> Result<Option<String>, FrameworkError> {
        let mut conn = self.conn.clone();
        let key = self.prefixed_key(key);

        let value: Option<String> = conn
            .get(&key)
            .await
            .map_err(|e| FrameworkError::internal(format!("Cache get error: {}", e)))?;

        Ok(value)
    }

    async fn put_raw(
        &self,
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<(), FrameworkError> {
        let mut conn = self.conn.clone();
        let key = self.prefixed_key(key);

        // `None` ttl means **no expiration** per the CacheStore contract.
        // The facade resolves any configured default before calling this
        // method — otherwise `Cache::forever` would not be forever on
        // Redis (HIGH audit finding #252).
        if let Some(duration) = ttl {
            conn.set_ex::<_, _, ()>(&key, value, duration.as_secs())
                .await
                .map_err(|e| FrameworkError::internal(format!("Cache set error: {}", e)))?;
        } else {
            conn.set::<_, _, ()>(&key, value)
                .await
                .map_err(|e| FrameworkError::internal(format!("Cache set error: {}", e)))?;
        }

        Ok(())
    }

    fn default_ttl(&self) -> Option<Duration> {
        self.default_ttl
    }

    async fn has(&self, key: &str) -> Result<bool, FrameworkError> {
        let mut conn = self.conn.clone();
        let key = self.prefixed_key(key);

        let exists: bool = conn
            .exists(&key)
            .await
            .map_err(|e| FrameworkError::internal(format!("Cache exists error: {}", e)))?;

        Ok(exists)
    }

    async fn forget(&self, key: &str) -> Result<bool, FrameworkError> {
        let mut conn = self.conn.clone();
        let key = self.prefixed_key(key);

        let deleted: i64 = conn
            .del(&key)
            .await
            .map_err(|e| FrameworkError::internal(format!("Cache delete error: {}", e)))?;

        Ok(deleted > 0)
    }

    async fn flush(&self) -> Result<(), FrameworkError> {
        let mut conn = self.conn.clone();

        // Use KEYS to find and delete all keys with our prefix
        // Note: KEYS is O(N) and should be used carefully in production
        let pattern = format!("{}*", self.prefix);
        let keys: Vec<String> = redis::cmd("KEYS")
            .arg(&pattern)
            .query_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("Cache flush scan error: {}", e)))?;

        if !keys.is_empty() {
            conn.del::<_, ()>(keys).await.map_err(|e| {
                FrameworkError::internal(format!("Cache flush delete error: {}", e))
            })?;
        }

        Ok(())
    }

    async fn increment(&self, key: &str, amount: i64) -> Result<i64, FrameworkError> {
        let mut conn = self.conn.clone();
        let key = self.prefixed_key(key);

        let value: i64 = conn
            .incr(&key, amount)
            .await
            .map_err(|e| FrameworkError::internal(format!("Cache increment error: {}", e)))?;

        Ok(value)
    }

    async fn decrement(&self, key: &str, amount: i64) -> Result<i64, FrameworkError> {
        let mut conn = self.conn.clone();
        let key = self.prefixed_key(key);

        let value: i64 = conn
            .decr(&key, amount)
            .await
            .map_err(|e| FrameworkError::internal(format!("Cache decrement error: {}", e)))?;

        Ok(value)
    }

    async fn tagged_put_raw(
        &self,
        tags: &[&str],
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<(), FrameworkError> {
        let mut conn = self.conn.clone();
        let pkey = self.prefixed_key(key);

        let mut pipe = redis::pipe();
        pipe.atomic();
        // `None` ttl honoured literally — see put_raw for rationale.
        if let Some(d) = ttl {
            pipe.cmd("SET")
                .arg(&pkey)
                .arg(value)
                .arg("EX")
                .arg(d.as_secs())
                .ignore();
        } else {
            pipe.cmd("SET").arg(&pkey).arg(value).ignore();
        }
        for t in tags {
            let tag_key = format!("{}tag:{}", self.prefix, t);
            pipe.cmd("SADD").arg(&tag_key).arg(&pkey).ignore();
        }
        pipe.query_async::<()>(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("Cache tagged set: {e}")))?;
        Ok(())
    }

    async fn flush_tags(&self, tags: &[&str]) -> Result<(), FrameworkError> {
        let mut conn = self.conn.clone();
        for t in tags {
            let tag_key = format!("{}tag:{}", self.prefix, t);
            let members: Vec<String> = redis::cmd("SMEMBERS")
                .arg(&tag_key)
                .query_async(&mut conn)
                .await
                .map_err(|e| FrameworkError::internal(format!("Cache tag scan: {e}")))?;
            if !members.is_empty() {
                redis::cmd("DEL")
                    .arg(members)
                    .query_async::<()>(&mut conn)
                    .await
                    .map_err(|e| FrameworkError::internal(format!("Cache tag flush: {e}")))?;
            }
            redis::cmd("DEL")
                .arg(&tag_key)
                .query_async::<()>(&mut conn)
                .await
                .map_err(|e| FrameworkError::internal(format!("Cache tag-index delete: {e}")))?;
        }
        Ok(())
    }

    async fn acquire_lock(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<Option<String>, FrameworkError> {
        let mut conn = self.conn.clone();
        let pkey = format!("{}lock:{}", self.prefix, key);
        let token = uuid::Uuid::new_v4().to_string();

        // SET key token NX EX ttl_secs — atomic: only sets if key does not exist
        let res: Option<String> = redis::cmd("SET")
            .arg(&pkey)
            .arg(&token)
            .arg("NX")
            .arg("EX")
            .arg(ttl.as_secs())
            .query_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("Lock acquire: {e}")))?;

        // Redis returns "OK" string on success, nil (None) on contention
        Ok(res.map(|_ok| token))
    }

    async fn release_lock(&self, key: &str, token: &str) -> Result<bool, FrameworkError> {
        let mut conn = self.conn.clone();
        let pkey = format!("{}lock:{}", self.prefix, key);
        // Atomically: if GET key == token then DEL key, else return 0
        let script = redis::Script::new(
            "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) else return 0 end",
        );
        let removed: i64 = script
            .key(&pkey)
            .arg(token)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("Lock release: {e}")))?;
        Ok(removed == 1)
    }

    async fn refresh_lock(
        &self,
        key: &str,
        token: &str,
        ttl: Duration,
    ) -> Result<bool, FrameworkError> {
        let mut conn = self.conn.clone();
        let pkey = format!("{}lock:{}", self.prefix, key);
        // Atomically: if GET key == token then EXPIRE key ttl, else return 0
        let script = redis::Script::new(
            "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('EXPIRE', KEYS[1], ARGV[2]) else return 0 end",
        );
        let ok: i64 = script
            .key(&pkey)
            .arg(token)
            .arg(ttl.as_secs() as i64)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("Lock refresh: {e}")))?;
        Ok(ok == 1)
    }

    async fn touch(&self, key: &str, ttl: Duration) -> Result<bool, FrameworkError> {
        let mut conn = self.conn.clone();
        let pkey = self.prefixed_key(key);
        // EXPIRE returns 1 if the TTL was set, 0 if the key does not exist
        let ok: i64 = redis::cmd("EXPIRE")
            .arg(&pkey)
            .arg(ttl.as_secs())
            .query_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("Cache touch: {e}")))?;
        Ok(ok == 1)
    }
}
