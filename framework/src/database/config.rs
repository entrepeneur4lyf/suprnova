//! Database configuration for suprnova framework

use crate::config::{env, env_optional, Environment};
use crate::FrameworkError;

/// Database type enumeration
#[derive(Debug, Clone, PartialEq)]
pub enum DatabaseType {
    Postgres,
    Mysql,
    Sqlite,
    Unknown,
}

/// Source provenance of [`DatabaseConfig::url`].
///
/// Tracks whether the URL came from the `DATABASE_URL` env variable
/// (`Env`), was filled in by the silent SQLite fallback (`Default`),
/// or was supplied explicitly via [`DatabaseConfigBuilder::url`]
/// (`Explicit`). Audit HIGH `database` #1: production boots must
/// refuse the silent fallback — see
/// [`DatabaseConfig::validate_for_environment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlSource {
    /// URL was read from the `DATABASE_URL` env var.
    Env,
    /// URL fell through to the dev-convenience `sqlite://./database.db`
    /// fallback because `DATABASE_URL` was unset.
    Default,
    /// URL was set programmatically (typically via the builder).
    Explicit,
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
    /// Where [`Self::url`] came from — env var, dev-fallback default,
    /// or an explicit programmatic value. Used by
    /// [`Self::validate_for_environment`] to refuse the silent
    /// SQLite fallback in production.
    pub url_source: UrlSource,
}

impl DatabaseConfig {
    /// The dev-convenience SQLite fallback URL used when
    /// `DATABASE_URL` is unset. Public so production
    /// preflight tooling can compare against it without
    /// hard-coding the string.
    pub const DEFAULT_SQLITE_URL: &'static str = "sqlite://./database.db";

    /// Create configuration from environment variables.
    ///
    /// When `DATABASE_URL` is unset, falls back to
    /// [`Self::DEFAULT_SQLITE_URL`] and records the source as
    /// [`UrlSource::Default`] — that flag is what
    /// [`Self::validate_for_environment`] uses to refuse the silent
    /// fallback in production.
    pub fn from_env() -> Self {
        let (url, url_source) = match env_optional("DATABASE_URL") {
            Some(u) => (u, UrlSource::Env),
            None => (Self::DEFAULT_SQLITE_URL.to_string(), UrlSource::Default),
        };
        Self {
            url,
            max_connections: env("DB_MAX_CONNECTIONS", 10),
            min_connections: env("DB_MIN_CONNECTIONS", 1),
            connect_timeout: env("DB_CONNECT_TIMEOUT", 30),
            logging: env("DB_LOGGING", false),
            url_source,
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

    /// Returns whether the database URL was explicitly configured
    /// rather than falling through to the dev SQLite default.
    ///
    /// Use this as a precondition signal — production boots that
    /// observe `false` here must refuse to continue. The lower-level
    /// helper that gates `DB::init` on this is
    /// [`Self::validate_for_environment`].
    pub fn is_configured(&self) -> bool {
        self.url_source != UrlSource::Default
    }

    /// Refuse to boot in a production-like environment when the URL
    /// fell through to the dev SQLite fallback.
    ///
    /// "Production-like" = [`Environment::Production`] or
    /// [`Environment::Staging`]. Local / Development / Testing /
    /// Custom environments keep the silent fallback for zero-setup
    /// iteration, matching the project's documented dev posture
    /// ("Suprnova dev default = SQLite").
    ///
    /// Called automatically by [`DB::init`](crate::DB::init) and
    /// [`DB::init_with`](crate::DB::init_with); manual `DB::init_with`
    /// callers that pre-build a config can call this themselves to
    /// fail-fast at config-creation time if they want a tighter
    /// guarantee.
    pub fn validate_for_environment(&self, env: &Environment) -> Result<(), FrameworkError> {
        let prod_like = env.is_production() || matches!(env, Environment::Staging);
        if prod_like && self.url_source == UrlSource::Default {
            return Err(FrameworkError::param(format!(
                "DATABASE_URL is required in `{env}` but was unset — refusing to boot \
                 against the dev SQLite fallback `{}`. Set DATABASE_URL to the \
                 production database URL, or construct an explicit config via \
                 `DatabaseConfig::builder().url(...)` when a SQLite file really is \
                 the production database.",
                Self::DEFAULT_SQLITE_URL,
            )));
        }
        Ok(())
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

    /// Build the configuration.
    ///
    /// `url`: if [`Self::url`] was called the resulting config
    /// carries [`UrlSource::Explicit`] (production-safe — the
    /// operator chose this URL deliberately). Otherwise the URL +
    /// source are inherited from
    /// [`DatabaseConfig::from_env`] — `Env` when `DATABASE_URL` is
    /// set, `Default` when it falls through to the SQLite
    /// convenience URL.
    pub fn build(self) -> DatabaseConfig {
        let defaults = DatabaseConfig::from_env();
        let (url, url_source) = match self.url {
            Some(u) => (u, UrlSource::Explicit),
            None => (defaults.url, defaults.url_source),
        };
        DatabaseConfig {
            url,
            max_connections: self.max_connections.unwrap_or(defaults.max_connections),
            min_connections: self.min_connections.unwrap_or(defaults.min_connections),
            connect_timeout: self.connect_timeout.unwrap_or(defaults.connect_timeout),
            logging: self.logging.unwrap_or(defaults.logging),
            url_source,
        }
    }
}
