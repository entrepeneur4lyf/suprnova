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
    /// Build config from environment variables.
    ///
    /// `APP_DEBUG` is environment-aware: if the variable is set, its
    /// explicit value wins; if unset, the default is derived from
    /// `APP_ENV` — `true` in local/development/testing, `false`
    /// otherwise (including production and any unrecognized
    /// environment). This keeps local zero-config DX while making
    /// production fail-safe per codex review finding #12.
    pub fn from_env() -> Self {
        let environment = Environment::detect();
        let debug = std::env::var("APP_DEBUG")
            .ok()
            .and_then(|s| s.parse::<bool>().ok())
            .unwrap_or_else(|| default_debug_for_env(&environment));

        Self {
            name: env("APP_NAME", "Suprnova Application".to_string()),
            environment,
            debug,
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

/// Pick the default value for `APP_DEBUG` when the env var is unset.
///
/// `true` in environments where developers want loud, helpful errors
/// (local, development, testing). `false` everywhere else — production,
/// staging, and any unrecognized custom environment fall closed.
fn default_debug_for_env(env: &Environment) -> bool {
    matches!(
        env,
        Environment::Local | Environment::Development | Environment::Testing
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_debug_is_true_in_local_dev_test() {
        assert!(default_debug_for_env(&Environment::Local));
        assert!(default_debug_for_env(&Environment::Development));
        assert!(default_debug_for_env(&Environment::Testing));
    }

    #[test]
    fn default_debug_is_false_in_production_staging_custom() {
        assert!(!default_debug_for_env(&Environment::Production));
        assert!(!default_debug_for_env(&Environment::Staging));
        assert!(!default_debug_for_env(&Environment::Custom("k8s-prod".into())));
    }
}
