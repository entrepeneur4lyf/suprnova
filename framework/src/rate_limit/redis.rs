//! Redis ZSET-backed sliding window. ZADD on acquire, ZREMRANGEBYSCORE
//! to evict, ZCARD to count. Atomic via a single Lua eval per call.

use crate::error::FrameworkError;
use crate::rate_limit::{RateLimiterDriver, SlidingWindowConfig};
use async_trait::async_trait;
use redis::Script;
use redis::aio::ConnectionManager;
use std::time::Duration;
use uuid::Uuid;

pub struct RedisRateLimiter {
    conn: ConnectionManager,
    prefix: String,
}

impl RedisRateLimiter {
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
        let mut conn = self.conn.clone();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let window_ms = config.window.as_millis() as i64;
        // Evict old entries then fetch the oldest score.
        redis::cmd("ZREMRANGEBYSCORE")
            .arg(&zkey)
            .arg("-inf")
            .arg(now_ms - window_ms)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("rl evict: {e}")))?;
        let count: i64 = redis::cmd("ZCARD")
            .arg(&zkey)
            .query_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("rl card: {e}")))?;
        if count < config.max_requests as i64 {
            return Ok(None);
        }
        let oldest: Vec<(String, f64)> = redis::cmd("ZRANGE")
            .arg(&zkey)
            .arg(0)
            .arg(0)
            .arg("WITHSCORES")
            .query_async(&mut conn)
            .await
            .map_err(|e| FrameworkError::internal(format!("rl range: {e}")))?;
        let oldest_score = oldest.first().map(|(_, s)| *s as i64).unwrap_or(now_ms);
        let elapsed_ms = (now_ms - oldest_score).max(0);
        let remaining_ms = (window_ms - elapsed_ms).max(0) as u64;
        Ok(Some(Duration::from_millis(remaining_ms)))
    }
}
