//! Cache configuration for suprnova framework

use crate::config::{env, env_optional};

/// Cache configuration
///
/// # Environment Variables
///
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
///     .url("redis://localhost:6379")
///     .prefix("myapp:")
///     .build());
/// ```
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Redis connection URL
    pub url: String,
    /// Key prefix for all cache entries
    pub prefix: String,
    /// Default TTL in seconds (0 = no expiration)
    pub default_ttl: u64,
}

impl CacheConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        Self {
            url: env_optional("REDIS_URL").unwrap_or_else(|| "redis://127.0.0.1:6379".to_string()),
            prefix: env("REDIS_PREFIX", "suprnova_cache:".to_string()),
            default_ttl: env("CACHE_DEFAULT_TTL", 3600),
        }
    }

    /// Create a builder for manual configuration
    pub fn builder() -> CacheConfigBuilder {
        CacheConfigBuilder::default()
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

/// Builder for CacheConfig
#[derive(Debug, Default)]
pub struct CacheConfigBuilder {
    url: Option<String>,
    prefix: Option<String>,
    default_ttl: Option<u64>,
}

impl CacheConfigBuilder {
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

    /// Build the configuration
    pub fn build(self) -> CacheConfig {
        let defaults = CacheConfig::from_env();
        CacheConfig {
            url: self.url.unwrap_or(defaults.url),
            prefix: self.prefix.unwrap_or(defaults.prefix),
            default_ttl: self.default_ttl.unwrap_or(defaults.default_ttl),
        }
    }
}
