//! Redis-backed cache implementation

use async_trait::async_trait;
use redis::{
    AsyncCommands, Client,
    aio::{ConnectionManager, ConnectionManagerConfig},
};
use std::time::Duration;

use super::config::CacheConfig;
use super::store::CacheStore;
use crate::error::FrameworkError;

/// Convert a `Duration` into a Redis-millisecond TTL argument.
///
/// Redis sub-second TTLs are expressed via `PX` (set) and `PEXPIRE`
/// (extend). Sub-second durations passed as `EX`/`EXPIRE` truncate to 0
/// seconds, which Redis rejects for `SET ... EX 0` and, worse, treats as
/// "delete the key" for `EXPIRE key 0`. Routing every Redis TTL through
/// `PX`/`PEXPIRE` (Redis 2.6+, 2012) avoids both pitfalls.
///
/// `Duration::ZERO` is clamped to 1 ms so neither `PX 0` (rejected) nor
/// `PEXPIRE 0` (key-delete) can sneak through. Caller-side `Duration`s
/// outside u64 ms (≈ 584 million years) saturate to `u64::MAX`; Redis
/// will reject that as an invalid expire on its own.
#[inline]
fn redis_ttl_ms(d: Duration) -> u64 {
    let ms = d.as_millis();
    if ms == 0 {
        1
    } else if ms > u64::MAX as u128 {
        u64::MAX
    } else {
        ms as u64
    }
}

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

        // Bound the initial-connect budget so an unreachable Redis fails
        // CLOSED promptly instead of hanging. The redis-rs
        // defaults are 6 reconnect retries with an UNCAPPED exponential
        // backoff (max_delay = None), so against a down/unreachable host the
        // connect future can take well over 10s to resolve with an error —
        // blocking `Cache::bootstrap` at startup for that whole window.
        //
        // We cap it: at most 3 retries, =<500ms between them, each connection
        // and command attempt bounded by an explicit timeout. A refused or
        // unreachable host now errors in under two seconds, while a healthy
        // Redis (sub-second on localhost/LAN) is unaffected.
        let cm_config = ConnectionManagerConfig::new()
            .set_connection_timeout(Some(Duration::from_secs(2)))
            .set_response_timeout(Some(Duration::from_secs(5)))
            .set_number_of_retries(3)
            .set_max_delay(Duration::from_millis(500));
        let conn = ConnectionManager::new_with_config(client, cm_config)
            .await
            .map_err(|e| {
                FrameworkError::internal(format!("Redis connection manager error: {e}"))
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

    /// Distributed-lock keyspace key for `key`.
    ///
    /// Locks live under a NUL-byte sentinel after the configured prefix
    /// so they cannot collide with any user-supplied cache key. User
    /// keys are always passed through `prefixed_key(...)` which does not
    /// inject the sentinel, so a caller doing `Cache::forget("lock:foo")`
    /// targets `<prefix>lock:foo` — distinct from the lock's
    /// `<prefix>\0lock:foo` slot. This prevents a regular `forget` /
    /// `put` from releasing or overwriting a held distributed lock.
    fn locked_key(&self, key: &str) -> String {
        format!("{}\0lock:{}", self.prefix, key)
    }

    /// Tag forward-index key (`tag -> set of value keys`).
    ///
    /// Hidden under the same NUL-byte sentinel as the lock keyspace so
    /// `Cache::forget("tag:users")` cannot drop the forward index for
    /// the `users` tag.
    fn tag_index_key(&self, tag: &str) -> String {
        format!("{}\0tag:{}", self.prefix, tag)
    }

    /// Aux SET that records the tag memberships for a value key.
    ///
    /// This lets `flush_tags` validate "is this key STILL tagged with `t`"
    /// at the moment of deletion, so an untagged overwrite of a previously
    /// tagged key is not silently deleted by a later `flush_tags(t)`.
    ///
    /// The aux set carries the same TTL as the value key, so an expired
    /// value's tag entries age out together rather than accumulating
    /// forever in the forward `tag:{t}` set.
    ///
    /// Stored under the same NUL-byte sentinel as the lock and tag
    /// forward index so the bookkeeping is unreachable from caller-side
    /// `Cache::put/forget/get`.
    fn key_tags_set(&self, prefixed_key: &str) -> String {
        format!("{}\0key_tags:{}", self.prefix, prefixed_key)
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
        let pkey = self.prefixed_key(key);
        let aux = self.key_tags_set(&pkey);

        // Drop any prior tag aux set so a later tagged_put_raw does not
        // resurrect stale tag memberships AND a later flush_tags cannot
        // delete this untagged value (the aux set is the source of truth
        // for "is this key still tagged with t?" at flush time). Pipelined
        // with the SET so an untagged write is still one round trip.
        let mut pipe = redis::pipe();
        pipe.atomic();
        pipe.cmd("DEL").arg(&aux).ignore();
        // `None` ttl means **no expiration** per the CacheStore contract.
        // The facade resolves any configured default before calling this
        // method — otherwise `Cache::forever` would not be forever on
        // Redis.
        if let Some(duration) = ttl {
            pipe.cmd("SET")
                .arg(&pkey)
                .arg(value)
                .arg("PX")
                .arg(redis_ttl_ms(duration))
                .ignore();
        } else {
            pipe.cmd("SET").arg(&pkey).arg(value).ignore();
        }
        pipe.query_async::<()>(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("Cache set error: {}", e)))?;
        Ok(())
    }

    fn default_ttl(&self) -> Option<Duration> {
        self.default_ttl
    }

    async fn add_raw(
        &self,
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<bool, FrameworkError> {
        let mut conn = self.conn.clone();
        let pkey = self.prefixed_key(key);

        // Atomic via SET NX [PX ttl] — Redis writes the value only when
        // the key does not exist. Returns the string "OK" on success and
        // nil (Option::None) on contention.
        let res: Option<String> = if let Some(d) = ttl {
            redis::cmd("SET")
                .arg(&pkey)
                .arg(value)
                .arg("NX")
                .arg("PX")
                .arg(redis_ttl_ms(d))
                .query_async(&mut conn)
                .await
                .map_err(|e| FrameworkError::internal(format!("Cache add error: {e}")))?
        } else {
            redis::cmd("SET")
                .arg(&pkey)
                .arg(value)
                .arg("NX")
                .query_async(&mut conn)
                .await
                .map_err(|e| FrameworkError::internal(format!("Cache add error: {e}")))?
        };

        // If we wrote a fresh untagged value, drop any leftover tag aux
        // set so a stale flush_tags cannot delete it.
        if res.is_some() {
            let aux = self.key_tags_set(&pkey);
            redis::cmd("DEL")
                .arg(&aux)
                .query_async::<()>(&mut conn)
                .await
                .map_err(|e| FrameworkError::internal(format!("Cache aux drop: {e}")))?;
        }

        Ok(res.is_some())
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
        let pkey = self.prefixed_key(key);

        // Drop the value AND its tag aux set. The forward `tag:{t}` set
        // may still list this key; that's harmless — flush_tags validates
        // membership via the aux set and skips a key whose aux set says
        // "no longer tagged with t" (or no longer exists at all).
        let aux = self.key_tags_set(&pkey);
        let deleted: i64 = redis::cmd("DEL")
            .arg(&pkey)
            .arg(&aux)
            .query_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("Cache delete error: {}", e)))?;

        // Return whether the value itself existed. The aux key tagging
        // along is bookkeeping — its prior absence doesn't make `forget`
        // a no-op from the caller's perspective.
        Ok(deleted > 0)
    }

    async fn flush(&self) -> Result<(), FrameworkError> {
        let mut conn = self.conn.clone();

        // SCAN beats KEYS for production: incremental cursor iteration
        // avoids blocking the Redis server on a single O(N) pass. We
        // batch DEL per page so very large keyspaces don't build one
        // giant argument list. The MATCH glob is anchored to our prefix
        // so we never touch other applications' keys.
        let pattern = format!("{}*", self.prefix);
        let mut cursor: u64 = 0;
        loop {
            let (next_cursor, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(500)
                .query_async(&mut conn)
                .await
                .map_err(|e| FrameworkError::internal(format!("Cache flush scan error: {}", e)))?;
            if !batch.is_empty() {
                conn.del::<_, ()>(batch).await.map_err(|e| {
                    FrameworkError::internal(format!("Cache flush delete error: {}", e))
                })?;
            }
            if next_cursor == 0 {
                break;
            }
            cursor = next_cursor;
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
        let aux = self.key_tags_set(&pkey);

        let mut pipe = redis::pipe();
        pipe.atomic();
        // Rewrite the aux set from scratch — replaces (not unions with)
        // any prior tag memberships. This is what protects a tagged
        // overwrite from carrying old tags.
        pipe.cmd("DEL").arg(&aux).ignore();
        // `None` ttl honoured literally — see put_raw for rationale.
        if let Some(d) = ttl {
            let pxms = redis_ttl_ms(d);
            pipe.cmd("SET")
                .arg(&pkey)
                .arg(value)
                .arg("PX")
                .arg(pxms)
                .ignore();
            // Aux set rides the same TTL so the bookkeeping ages out with
            // the value rather than accumulating forever.
            if !tags.is_empty() {
                let mut sadd = redis::cmd("SADD");
                sadd.arg(&aux);
                for t in tags {
                    sadd.arg(*t);
                }
                pipe.add_command(sadd).ignore();
                pipe.cmd("PEXPIRE").arg(&aux).arg(pxms).ignore();
            }
        } else {
            pipe.cmd("SET").arg(&pkey).arg(value).ignore();
            if !tags.is_empty() {
                let mut sadd = redis::cmd("SADD");
                sadd.arg(&aux);
                for t in tags {
                    sadd.arg(*t);
                }
                pipe.add_command(sadd).ignore();
            }
        }
        // Forward index: tag -> set of value keys. Used as the candidate
        // list by flush_tags; the aux set is the source of truth for
        // "is this key still tagged with t" at deletion time.
        for t in tags {
            let tag_key = self.tag_index_key(t);
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
            let tag_key = self.tag_index_key(t);
            let members: Vec<String> = redis::cmd("SMEMBERS")
                .arg(&tag_key)
                .query_async(&mut conn)
                .await
                .map_err(|e| FrameworkError::internal(format!("Cache tag scan: {e}")))?;
            for member in members {
                let aux = self.key_tags_set(&member);
                // SISMEMBER is the validation gate: if the key's aux set
                // no longer contains this tag — because the key was
                // overwritten untagged, or the aux set already expired
                // alongside the value — we leave the value alone and
                // just prune the forward index entry.
                let still_tagged: bool = redis::cmd("SISMEMBER")
                    .arg(&aux)
                    .arg(*t)
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| FrameworkError::internal(format!("Cache tag check: {e}")))?;
                if still_tagged {
                    // DEL the value AND its aux set. Other tags that
                    // referenced the same key still get forward-index
                    // pruning on their own flush via the SISMEMBER gate
                    // (which will now miss because the aux set is gone).
                    redis::cmd("DEL")
                        .arg(&member)
                        .arg(&aux)
                        .query_async::<()>(&mut conn)
                        .await
                        .map_err(|e| FrameworkError::internal(format!("Cache tag flush: {e}")))?;
                }
            }
            // Forward index always cleared — its job here is done. Any
            // residual references in OTHER tags' forward sets will be
            // SISMEMBER-skipped on a future flush.
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
        let pkey = self.locked_key(key);
        let token = uuid::Uuid::new_v4().to_string();

        // SET key token NX PX ttl_ms — atomic: only sets if key does not
        // exist. PX preserves sub-second precision (EX truncates and a
        // sub-second TTL would round to 0, which Redis rejects).
        let res: Option<String> = redis::cmd("SET")
            .arg(&pkey)
            .arg(&token)
            .arg("NX")
            .arg("PX")
            .arg(redis_ttl_ms(ttl))
            .query_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("Lock acquire: {e}")))?;

        // Redis returns "OK" string on success, nil (None) on contention
        Ok(res.map(|_ok| token))
    }

    async fn release_lock(&self, key: &str, token: &str) -> Result<bool, FrameworkError> {
        let mut conn = self.conn.clone();
        let pkey = self.locked_key(key);
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
        let pkey = self.locked_key(key);
        // Atomically: if GET key == token then PEXPIRE key ttl_ms, else
        // return 0. PEXPIRE preserves sub-second precision — EXPIRE
        // would truncate, and `EXPIRE key 0` deletes the key, which
        // would silently release the lock on a sub-second refresh.
        let script = redis::Script::new(
            "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('PEXPIRE', KEYS[1], ARGV[2]) else return 0 end",
        );
        let ok: i64 = script
            .key(&pkey)
            .arg(token)
            .arg(redis_ttl_ms(ttl) as i64)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("Lock refresh: {e}")))?;
        Ok(ok == 1)
    }

    async fn touch(&self, key: &str, ttl: Duration) -> Result<bool, FrameworkError> {
        let mut conn = self.conn.clone();
        let pkey = self.prefixed_key(key);
        // PEXPIRE returns 1 if the TTL was set, 0 if the key does not
        // exist. PEXPIRE preserves sub-second precision; EXPIRE would
        // truncate a sub-second ttl to 0 and delete the key.
        let ok: i64 = redis::cmd("PEXPIRE")
            .arg(&pkey)
            .arg(redis_ttl_ms(ttl))
            .query_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("Cache touch: {e}")))?;
        Ok(ok == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redis_ttl_ms_preserves_millisecond_resolution() {
        assert_eq!(redis_ttl_ms(Duration::from_millis(1)), 1);
        assert_eq!(redis_ttl_ms(Duration::from_millis(50)), 50);
        assert_eq!(redis_ttl_ms(Duration::from_millis(999)), 999);
        assert_eq!(redis_ttl_ms(Duration::from_secs(1)), 1_000);
        assert_eq!(redis_ttl_ms(Duration::from_secs(60)), 60_000);
    }

    #[test]
    fn redis_ttl_ms_clamps_zero_to_one_ms() {
        // Redis rejects PX 0 and PEXPIRE key 0 deletes the key — clamp
        // to 1 ms so neither failure mode is reachable from this layer.
        assert_eq!(redis_ttl_ms(Duration::ZERO), 1);
    }

    #[test]
    fn redis_ttl_ms_handles_large_durations_safely() {
        // 1 year in ms fits comfortably in u64; verify the path.
        let one_year_ms = 365u64 * 24 * 60 * 60 * 1000;
        assert_eq!(
            redis_ttl_ms(Duration::from_secs(365 * 24 * 60 * 60)),
            one_year_ms
        );
        // u64::MAX milliseconds is a hard ceiling — anything past it
        // saturates rather than wrapping or panicking.
        assert_eq!(redis_ttl_ms(Duration::MAX), u64::MAX);
    }

    #[test]
    fn redis_ttl_ms_subsecond_does_not_round_to_zero() {
        // The bug we're fixing: `as_secs()` of any sub-second Duration is
        // 0. Verify the replacement preserves precision instead.
        let half_sec = Duration::from_millis(500);
        assert_eq!(half_sec.as_secs(), 0, "control: as_secs truncates");
        assert_eq!(redis_ttl_ms(half_sec), 500, "as_millis preserves");
    }
}
