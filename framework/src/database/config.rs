//! Database configuration for suprnova framework

use crate::config::{env, env_optional};

/// Database type enumeration
#[derive(Debug, Clone, PartialEq)]
pub enum DatabaseType {
    Postgres,
    Mysql,
    Sqlite,
    Unknown,
}

/// Database configuration
///
/// # Environment Variables
///
/// - `DATABASE_URL` - Full connection URL (required for connection, defaults to sqlite://./database.db)
/// - `DB_MAX_CONNECTIONS` - Maximum pool connections (default: 10)
/// - `DB_MIN_CONNECTIONS` - Minimum pool connections (default: 1)
/// - `DB_CONNECT_TIMEOUT` - Connection timeout in seconds (default: 30)
/// - `DB_LOGGING` - Enable SQL logging (default: false)
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{Config, DatabaseConfig};
///
/// // Register from environment
/// Config::register(DatabaseConfig::from_env());
///
/// // Or build manually
/// Config::register(DatabaseConfig::builder()
///     .url("postgres://localhost/mydb")
///     .max_connections(20)
///     .build());
/// ```
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// Full database connection URL
    pub url: String,
    /// Maximum connections in pool
    pub max_connections: u32,
    /// Minimum connections in pool
    pub min_connections: u32,
    /// Connection timeout in seconds
    pub connect_timeout: u64,
    /// Enable SQL query logging
    pub logging: bool,
}

impl DatabaseConfig {
    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        Self {
            url: env_optional("DATABASE_URL")
                .unwrap_or_else(|| "sqlite://./database.db".to_string()),
            max_connections: env("DB_MAX_CONNECTIONS", 10),
            min_connections: env("DB_MIN_CONNECTIONS", 1),
            connect_timeout: env("DB_CONNECT_TIMEOUT", 30),
            logging: env("DB_LOGGING", false),
        }
    }

    /// Create a builder for manual configuration
    pub fn builder() -> DatabaseConfigBuilder {
        DatabaseConfigBuilder::default()
    }

    /// Detect database type from URL
    pub fn database_type(&self) -> DatabaseType {
        if self.url.starts_with("postgres://") || self.url.starts_with("postgresql://") {
            DatabaseType::Postgres
        } else if self.url.starts_with("mysql://") {
            DatabaseType::Mysql
        } else if self.url.starts_with("sqlite://") || self.url.starts_with("sqlite:") {
            DatabaseType::Sqlite
        } else {
            DatabaseType::Unknown
        }
    }

    /// Check if database URL is configured (not the default)
    pub fn is_configured(&self) -> bool {
        self.url != "sqlite://./database.db"
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

/// Builder for DatabaseConfig
#[derive(Debug, Default)]
pub struct DatabaseConfigBuilder {
    url: Option<String>,
    max_connections: Option<u32>,
    min_connections: Option<u32>,
    connect_timeout: Option<u64>,
    logging: Option<bool>,
}

impl DatabaseConfigBuilder {
    /// Set the database URL
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Set maximum pool connections
    pub fn max_connections(mut self, count: u32) -> Self {
        self.max_connections = Some(count);
        self
    }

    /// Set minimum pool connections
    pub fn min_connections(mut self, count: u32) -> Self {
        self.min_connections = Some(count);
        self
    }

    /// Set connection timeout in seconds
    pub fn connect_timeout(mut self, seconds: u64) -> Self {
        self.connect_timeout = Some(seconds);
        self
    }

    /// Enable or disable SQL logging
    pub fn logging(mut self, enabled: bool) -> Self {
        self.logging = Some(enabled);
        self
    }

    /// Build the configuration
    pub fn build(self) -> DatabaseConfig {
        let defaults = DatabaseConfig::from_env();
        DatabaseConfig {
            url: self.url.unwrap_or(defaults.url),
            max_connections: self.max_connections.unwrap_or(defaults.max_connections),
            min_connections: self.min_connections.unwrap_or(defaults.min_connections),
            connect_timeout: self.connect_timeout.unwrap_or(defaults.connect_timeout),
            logging: self.logging.unwrap_or(defaults.logging),
        }
    }
}
