use crate::config::env::{env, Environment};

/// Application configuration
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Application name
    pub name: String,
    /// Current environment
    pub environment: Environment,
    /// Debug mode enabled
    pub debug: bool,
    /// Application URL
    pub url: String,
}

impl AppConfig {
    /// Build config from environment variables
    pub fn from_env() -> Self {
        Self {
            name: env("APP_NAME", "suprnova Application".to_string()),
            environment: Environment::detect(),
            debug: env("APP_DEBUG", true),
            url: env("APP_URL", "http://localhost:8080".to_string()),
        }
    }

    /// Create a builder for customizing config
    pub fn builder() -> AppConfigBuilder {
        AppConfigBuilder::default()
    }

    /// Check if debug mode is enabled
    pub fn is_debug(&self) -> bool {
        self.debug
    }

    /// Check if running in production
    pub fn is_production(&self) -> bool {
        self.environment.is_production()
    }

    /// Check if running in development
    pub fn is_development(&self) -> bool {
        self.environment.is_development()
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

/// Builder for AppConfig
#[derive(Default)]
pub struct AppConfigBuilder {
    name: Option<String>,
    environment: Option<Environment>,
    debug: Option<bool>,
    url: Option<String>,
}

impl AppConfigBuilder {
    /// Set the application name
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the environment
    pub fn environment(mut self, env: Environment) -> Self {
        self.environment = Some(env);
        self
    }

    /// Set debug mode
    pub fn debug(mut self, debug: bool) -> Self {
        self.debug = Some(debug);
        self
    }

    /// Set the application URL
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Build the AppConfig
    pub fn build(self) -> AppConfig {
        let default = AppConfig::from_env();
        AppConfig {
            name: self.name.unwrap_or(default.name),
            environment: self.environment.unwrap_or(default.environment),
            debug: self.debug.unwrap_or(default.debug),
            url: self.url.unwrap_or(default.url),
        }
    }
}
