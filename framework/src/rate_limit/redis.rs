//! Redis ZSET-backed sliding window. ZADD on acquire, ZREMRANGEBYSCORE
//! to evict, ZCARD to count. Atomic via a single Lua eval per call.

use crate::error::FrameworkError;
use crate::rate_limit::{RateLimiterDriver, SlidingWindowConfig};
use async_trait::async_trait;
use redis::Script;
use redis::aio::ConnectionManager;
use std::time::Duration;
use uuid::Uuid;

/// Redis-backed sliding-window rate limiter. Stores hit timestamps in a
/// ZSET per key and prunes them atomically via a single Lua eval.
pub struct RedisRateLimiter {
    conn: ConnectionManager,
    prefix: String,
}

impl RedisRateLimiter {
    /// Open a connection to `url` and return a limiter that scopes
    /// every key under `prefix`.
    pub async fn connect(url: &str, prefix: &str) -> Result<Self, FrameworkError> {
        let client = redis::Client::open(url)
            .map_err(|e| FrameworkError::internal(format!("redis open: {e}")))?;
        let conn = ConnectionManager::new(client)
            .await
            .map_err(|e| FrameworkError::internal(format!("redis conn: {e}")))?;
        Ok(Self {
            conn,
            prefix: prefix.into(),
        })
    }
}

#[async_trait]
impl RateLimiterDriver for RedisRateLimiter {
    async fn try_acquire(
        &self,
        key: &str,
        config: &SlidingWindowConfig,
    ) -> Result<bool, FrameworkError> {
        let zkey = format!("{}rl:{}", self.prefix, key);
        let now_ms = chrono::Utc::now().timestamp_millis();
        let window_ms = config.window.as_millis() as i64;
        let member = Uuid::new_v4().to_string();

        let script = Script::new(
            r"
            local zkey = KEYS[1]
            local now = tonumber(ARGV[1])
            local window = tonumber(ARGV[2])
            local max = tonumber(ARGV[3])
            local member = ARGV[4]
            redis.call('ZREMRANGEBYSCORE', zkey, '-inf', now - window)
            local count = redis.call('ZCARD', zkey)
            if count < max then
                redis.call('ZADD', zkey, now, member)
                redis.call('PEXPIRE', zkey, window)
                return 1
            else
                return 0
            end
        ",
        );
        let mut conn = self.conn.clone();
        let ok: i64 = script
            .key(&zkey)
            .arg(now_ms)
            .arg(window_ms)
            .arg(config.max_requests as i64)
            .arg(member)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("rate limit script: {e}")))?;
        Ok(ok == 1)
    }

    async fn retry_after(
        &self,
        key: &str,
        config: &SlidingWindowConfig,
    ) -> Result<Option<Duration>, FrameworkError> {
        let zkey = format!("{}rl:{}", self.prefix, key);
        let now_ms = chrono::Utc::now().timestamp_millis();
        let window_ms = config.window.as_millis() as i64;

        // Single Lua block so the evict / count / oldest-score reads
        // observe the same snapshot. As three separate round-trips a
        // concurrent `try_acquire` (which is itself atomic) could
        // ZADD between our ZCARD and our ZRANGE — count says "at
        // limit" then ZRANGE returns a *newer* member's score,
        // shrinking the computed Retry-After well below the real
        // remaining window. Returns -1 for "under limit, no header
        // needed" and a non-negative ms count for "still throttled,
        // header should be at least this long."
        let script = Script::new(
            r"
            local zkey = KEYS[1]
            local now = tonumber(ARGV[1])
            local window = tonumber(ARGV[2])
            local max = tonumber(ARGV[3])
            redis.call('ZREMRANGEBYSCORE', zkey, '-inf', now - window)
            local count = redis.call('ZCARD', zkey)
            if count < max then
                return -1
            end
            local oldest = redis.call('ZRANGE', zkey, 0, 0, 'WITHSCORES')
            if #oldest < 2 then
                return 0
            end
            local oldest_score = tonumber(oldest[2])
            local elapsed = now - oldest_score
            if elapsed < 0 then elapsed = 0 end
            local remaining = window - elapsed
            if remaining < 0 then remaining = 0 end
            return remaining
            ",
        );
        let mut conn = self.conn.clone();
        let remaining_ms: i64 = script
            .key(&zkey)
            .arg(now_ms)
            .arg(window_ms)
            .arg(config.max_requests as i64)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("rl retry_after script: {e}")))?;
        if remaining_ms < 0 {
            return Ok(None);
        }
        Ok(Some(Duration::from_millis(remaining_ms as u64)))
    }
}
