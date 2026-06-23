//! Configuration module for suprnova framework
//!
//! This module provides Laravel-like configuration management including:
//! - Automatic `.env` file loading with environment-based precedence
//! - Type-safe configuration structs
//! - Simple API for accessing config values
//!
//! # Example
//!
//! ```rust,no_run
//! use suprnova::{Config, ServerConfig};
//!
//! fn main() -> Result<(), suprnova::error::FrameworkError> {
//!     // Initialize config (loads .env files). Boot fails loudly if a
//!     // `.env` file is malformed or a typed env var fails to parse.
//!     Config::init(std::path::Path::new("."))?;
//!
//!     // Get typed config
//!     let server = Config::get::<ServerConfig>().unwrap();
//!     println!("Server port: {}", server.port);
//!     Ok(())
//! }
//! ```

pub mod env;
pub mod providers;
pub mod repository;
pub mod typed;

#[doc(hidden)]
pub use env::__reset_loaded_keys_for_tests;
pub use env::{Environment, env, env_optional, env_required, load_dotenv, try_env_required};
pub use providers::{AppConfig, AppConfigBuilder, ServerConfig, ServerConfigBuilder};

use std::path::Path;

/// Main Config facade for accessing configuration
///
/// The Config struct provides a centralized way to initialize and access
/// application configuration. It follows the Laravel pattern of type-safe
/// configuration with environment variable support.
pub struct Config;

impl Config {
    /// Initialize the configuration system
    ///
    /// This should be called at application startup, before creating the server.
    /// It loads environment variables from `.env` files and registers default configs.
    ///
    /// # Arguments
    ///
    /// * `project_root` - Path to the project root where `.env` files are located
    ///
    /// # Returns
    ///
    /// The detected environment (Local, Development, Production, etc.)
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::FrameworkError`] when a discovered
    /// `.env` file cannot be read or parsed, or when a typed
    /// framework knob (e.g. `SERVER_PORT`, `APP_DEBUG`) is set to a
    /// value that fails to parse. Missing `.env` files are not an
    /// error.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use suprnova::Config;
    ///
    /// let env = Config::init(std::path::Path::new("."))
    ///     .expect("config init");
    /// println!("Running in {} environment", env);
    /// ```
    pub fn init(project_root: &Path) -> Result<Environment, crate::error::FrameworkError> {
        let env = env::load_dotenv(project_root)?;

        // Register default configs, using the strict variants so a
        // typo in `SERVER_PORT` or `APP_DEBUG` aborts boot loudly
        // instead of silently falling back to the default.
        repository::register(AppConfig::try_from_env()?);
        repository::register(ServerConfig::try_from_env()?);

        Ok(env)
    }

    /// Get a typed config struct from the repository
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use suprnova::{Config, ServerConfig};
    ///
    /// let server_config = Config::get::<ServerConfig>().unwrap();
    /// println!("Port: {}", server_config.port);
    /// ```
    pub fn get<T: std::any::Any + Send + Sync + Clone + 'static>() -> Option<T> {
        repository::get::<T>()
    }

    /// Register a custom config struct
    ///
    /// Use this to register your own configuration structs that can be
    /// retrieved later with `Config::get::<T>()`.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use suprnova::Config;
    ///
    /// #[derive(Clone)]
    /// struct DatabaseConfig {
    ///     host: String,
    ///     port: u16,
    /// }
    ///
    /// Config::register(DatabaseConfig {
    ///     host: "localhost".to_string(),
    ///     port: 5432,
    /// });
    /// ```
    pub fn register<T: std::any::Any + Send + Sync + 'static>(config: T) {
        repository::register(config);
    }

    /// Check if a config type is registered
    pub fn has<T: std::any::Any + 'static>() -> bool {
        repository::has::<T>()
    }

    /// Get the current environment
    ///
    /// Returns the environment from AppConfig if initialized,
    /// otherwise detects from the APP_ENV environment variable.
    pub fn environment() -> Environment {
        Config::get::<AppConfig>()
            .map(|c| c.environment)
            .unwrap_or_else(Environment::detect)
    }

    /// Check if running in production environment
    pub fn is_production() -> bool {
        Self::environment().is_production()
    }

    /// Check if running in development environment (local or development)
    pub fn is_development() -> bool {
        Self::environment().is_development()
    }

    /// Check if debug mode is enabled.
    ///
    /// Resolution order: a programmatically-registered `AppConfig` wins;
    /// otherwise we fall back to `AppConfig::from_env()`, which reads
    /// `APP_DEBUG` and — if that env var is also unset — applies the
    /// env-aware default (true in Local/Development/Testing, false
    /// elsewhere). This keeps loud-by-default DX on the
    /// repository-not-yet-seeded boot/test path while staying fail-closed
    /// in production-shaped environments. The previous fallback was a
    /// hardcoded `true`, which silently leaked `debug_message` bodies
    /// from the JSON error renderers on uninitialized paths.
    pub fn is_debug() -> bool {
        Config::get::<AppConfig>()
            .unwrap_or_else(AppConfig::from_env)
            .is_debug()
    }

    /// Deserialize the current process's environment into a typed
    /// config struct via [`envy`]. Field names map to env vars
    /// UPPER_SNAKE — `pub mail_host: String` reads `MAIL_HOST`. Use
    /// `#[serde(default = "...")]` for fallbacks and
    /// `#[serde(rename = "...")]` to override the env-var name.
    ///
    /// ```rust,no_run
    /// # fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// #[derive(serde::Deserialize)]
    /// struct MailConfig {
    ///     pub mail_driver: String,
    ///     pub mail_host: String,
    ///     #[serde(default = "default_port")]
    ///     pub mail_port: u16,
    /// }
    /// fn default_port() -> u16 { 587 }
    ///
    /// let cfg: MailConfig = suprnova::Config::resolve()?;
    /// # Ok(()) }
    /// ```
    pub fn resolve<T: serde::de::DeserializeOwned>() -> Result<T, crate::error::FrameworkError> {
        typed::resolve()
    }

    /// Like [`Config::resolve`] but only reads env vars starting with
    /// `prefix`. The prefix is stripped before mapping to struct
    /// fields: `Config::resolve_prefixed::<MailCfg>("MAIL_")` + a
    /// `pub host: String` field reads `MAIL_HOST`.
    pub fn resolve_prefixed<T: serde::de::DeserializeOwned>(
        prefix: &str,
    ) -> Result<T, crate::error::FrameworkError> {
        typed::resolve_prefixed(prefix)
    }
}
