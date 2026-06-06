//! Cache configuration for suprnova framework

use crate::config::{env, env_optional};
use crate::error::FrameworkError;

/// Which cache backend to bootstrap.
///
/// Selected via the `CACHE_DRIVER` env var (parsed case-insensitively).
/// Defaults to [`CacheDriver::Memory`] when unset — single-process dev
/// loops don't need Redis, and the previous "try Redis, silently fall
/// back to memory on failure" behaviour was actively dangerous in
/// production. Operators choosing Redis MUST set the driver explicitly
/// so connection failures surface at boot instead of silently degrading
/// to per-process in-memory state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheDriver {
    /// Per-process in-memory cache. Default. No external dependencies.
    #[default]
    Memory,
    /// Redis-backed cache. Reads `REDIS_URL`. Boot fails closed if the
    /// configured URL is unreachable.
    Redis,
}

impl CacheDriver {
    /// Parse a `CACHE_DRIVER` env-var value. Case-insensitive; trims
    /// whitespace; returns an `internal` error for unknown driver names
    /// so misconfigurations surface at boot.
    pub fn parse(s: &str) -> Result<Self, FrameworkError> {
        match s.trim().to_ascii_lowercase().as_str() {
            "memory" | "in-memory" | "inmemory" => Ok(Self::Memory),
            "redis" => Ok(Self::Redis),
            other => Err(FrameworkError::internal(format!(
                "CACHE_DRIVER: unknown driver `{other}` (expected `memory` or `redis`)"
            ))),
        }
    }
}

/// Cache configuration
///
/// # Environment Variables
///
/// - `CACHE_DRIVER` — `memory` (default) or `redis`. Selects the
///   bootstrap target. Memory keeps everything in this process; Redis
///   requires `REDIS_URL` and fails boot if unreachable.
/// - `REDIS_URL` - Redis connection URL (default: redis://127.0.0.1:6379)
/// - `REDIS_PREFIX` - Key prefix for cache entries (default: "suprnova_cache:")
/// - `CACHE_DEFAULT_TTL` - Default TTL in seconds, 0 = no expiration (default: 3600)
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{Config, CacheConfig};
///
/// // Register from environment
/// Config::register(CacheConfig::from_env());
///
/// // Or build manually
/// Config::register(CacheConfig::builder()
///     .driver(CacheDriver::Redis)
///     .url("redis://localhost:6379")
///     .prefix("myapp:")
///     .build());
/// ```
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Which backend to bootstrap. Defaults to in-memory.
    pub driver: CacheDriver,
    /// Redis connection URL (consulted only when `driver == Redis`)
    pub url: String,
    /// Key prefix for all cache entries
    pub prefix: String,
    /// Default TTL in seconds (0 = no expiration). The facade applies
    /// this to `Cache::put(None)` / `Cache::tags_put(None)`. `Cache::forever`
    /// and `Cache::remember_forever` always bypass it.
    pub default_ttl: u64,
}

impl CacheConfig {
    /// Create configuration from environment variables.
    ///
    /// # Errors
    ///
    /// Returns an internal error if `CACHE_DRIVER` is set to an unknown
    /// value. Unset means "use the default", which is in-memory.
    pub fn from_env() -> Result<Self, FrameworkError> {
        let driver = match env_optional::<String>("CACHE_DRIVER") {
            Some(s) => CacheDriver::parse(&s)?,
            None => CacheDriver::default(),
        };
        Ok(Self {
            driver,
            url: env_optional("REDIS_URL").unwrap_or_else(|| "redis://127.0.0.1:6379".to_string()),
            prefix: env("REDIS_PREFIX", "suprnova_cache:".to_string()),
            default_ttl: env("CACHE_DEFAULT_TTL", 3600),
        })
    }

    /// Create a builder for manual configuration
    pub fn builder() -> CacheConfigBuilder {
        CacheConfigBuilder::default()
    }
}

impl Default for CacheConfig {
    /// In-memory driver, framework default URL/prefix/TTL — designed to
    /// succeed without env vars set. Use `CacheConfig::from_env()` (which
    /// returns a `Result`) when the caller wants `CACHE_DRIVER` parsing
    /// errors to surface.
    fn default() -> Self {
        Self {
            driver: CacheDriver::Memory,
            url: "redis://127.0.0.1:6379".to_string(),
            prefix: "suprnova_cache:".to_string(),
            default_ttl: 3600,
        }
    }
}

/// Builder for CacheConfig
#[derive(Debug, Default)]
pub struct CacheConfigBuilder {
    driver: Option<CacheDriver>,
    url: Option<String>,
    prefix: Option<String>,
    default_ttl: Option<u64>,
}

impl CacheConfigBuilder {
    /// Pick the backend explicitly.
    pub fn driver(mut self, driver: CacheDriver) -> Self {
        self.driver = Some(driver);
        self
    }

    /// Set the Redis URL
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Set the key prefix
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// Set the default TTL in seconds
    pub fn default_ttl(mut self, seconds: u64) -> Self {
        self.default_ttl = Some(seconds);
        self
    }

    /// Build the configuration. Falls back to `CacheConfig::default()`
    /// for any unset field rather than re-reading the environment, so
    /// the builder is fully deterministic.
    pub fn build(self) -> CacheConfig {
        let defaults = CacheConfig::default();
        CacheConfig {
            driver: self.driver.unwrap_or(defaults.driver),
            url: self.url.unwrap_or(defaults.url),
            prefix: self.prefix.unwrap_or(defaults.prefix),
            default_ttl: self.default_ttl.unwrap_or(defaults.default_ttl),
        }
    }
}
