use crate::config::env::{Environment, env, env_strict};
use crate::error::FrameworkError;

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
    /// production fail-safe.
    ///
    /// This helper is lenient — a typo in `APP_DEBUG` falls back to
    /// the environment-derived default (with a `tracing::warn!`).
    /// It is used by `impl Default`, the builder fallback path, and
    /// the lenient `Config::is_debug` fallback. The strict variant
    /// is [`Self::try_from_env`]; `Config::init` calls that.
    pub fn from_env() -> Self {
        let environment = Environment::detect();
        let debug = match std::env::var("APP_DEBUG") {
            Ok(raw) => match raw.parse::<bool>() {
                Ok(v) => v,
                Err(_) => {
                    tracing::warn!(
                        env_var = "APP_DEBUG",
                        raw_value = %raw,
                        "APP_DEBUG is set but failed to parse as bool; \
                         falling back to environment-derived default"
                    );
                    default_debug_for_env(&environment)
                }
            },
            Err(_) => default_debug_for_env(&environment),
        };

        Self {
            name: env("APP_NAME", "Suprnova Application".to_string()),
            environment,
            debug,
            url: env("APP_URL", "http://localhost:8080".to_string()),
        }
    }

    /// Build config from environment variables, returning an error if
    /// any typed knob is set to a value that fails to parse. Used by
    /// `Config::init` so a typo in `APP_DEBUG` aborts boot instead
    /// of silently reverting to the env-derived default.
    pub fn try_from_env() -> Result<Self, FrameworkError> {
        let environment = Environment::detect();
        let debug =
            env_strict::<bool>("APP_DEBUG")?.unwrap_or_else(|| default_debug_for_env(&environment));
        let name =
            env_strict::<String>("APP_NAME")?.unwrap_or_else(|| "Suprnova Application".to_string());
        let url =
            env_strict::<String>("APP_URL")?.unwrap_or_else(|| "http://localhost:8080".to_string());
        Ok(Self {
            name,
            environment,
            debug,
            url,
        })
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
    #[serial_test::serial(app_config_env)]
    fn try_from_env_rejects_unparseable_debug() {
        // `APP_DEBUG=not-a-bool` must fail boot via `try_from_env`,
        // not silently fall back to the environment-derived default
        // the way the lenient `from_env` path does. This is the
        // boot-time fail-loud guarantee `Config::init` relies on.
        let prior = std::env::var("APP_DEBUG").ok();
        // SAFETY: this test mutates a process-global env var. Other
        // tests in this crate use the same single-threaded pattern;
        // we restore the prior value at the end.
        unsafe {
            std::env::set_var("APP_DEBUG", "not-a-bool");
        }
        let err = AppConfig::try_from_env().expect_err("unparseable debug must error");
        let msg = format!("{}", err);
        assert!(
            msg.contains("APP_DEBUG"),
            "error should name the env var: {:?}",
            msg
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("APP_DEBUG", v),
                None => std::env::remove_var("APP_DEBUG"),
            }
        }
    }

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
        assert!(!default_debug_for_env(&Environment::Custom(
            "k8s-prod".into()
        )));
    }
}
