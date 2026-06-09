use crate::config::env::{Environment, env, env_strict};
use crate::error::FrameworkError;
use crate::http::TrustedProxiesConfig;
use std::net::IpAddr;

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
    /// Allowlist of TCP peer addresses whose `X-Forwarded-*` and
    /// `X-Real-IP` headers may be trusted by
    /// [`Request::ip`](crate::Request::ip) and the related host /
    /// scheme / port accessors.
    ///
    /// Defaults to an **empty** allowlist — proxy headers are ignored
    /// on every request unless the operator opts in. Behind a real
    /// terminating proxy (nginx, ALB, Cloudflare), list the addresses
    /// from which the proxy hops can reach the framework.
    ///
    /// Reads `APP_TRUSTED_PROXIES` from the environment as a
    /// comma-separated list of IP addresses, e.g.
    /// `APP_TRUSTED_PROXIES=127.0.0.1,10.0.0.1`. An unparseable entry
    /// fails boot via [`Self::try_from_env`].
    pub trusted_proxies: TrustedProxiesConfig,
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
            url: env("APP_URL", "http://localhost:8765".to_string()),
            trusted_proxies: parse_trusted_proxies_lenient(),
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
            env_strict::<String>("APP_URL")?.unwrap_or_else(|| "http://localhost:8765".to_string());
        let trusted_proxies = parse_trusted_proxies_strict()?;
        Ok(Self {
            name,
            environment,
            debug,
            url,
            trusted_proxies,
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

/// Parse `APP_TRUSTED_PROXIES` lenient — bad entries fall back to an
/// empty allowlist with a `tracing::warn!`. Used by `from_env`.
fn parse_trusted_proxies_lenient() -> TrustedProxiesConfig {
    let Ok(raw) = std::env::var("APP_TRUSTED_PROXIES") else {
        return TrustedProxiesConfig::empty();
    };
    match parse_ip_list(&raw) {
        Ok(ips) => TrustedProxiesConfig::with_ips(ips),
        Err(bad) => {
            tracing::warn!(
                env_var = "APP_TRUSTED_PROXIES",
                bad_entry = %bad,
                "APP_TRUSTED_PROXIES contains an unparseable IP; falling back to empty allowlist"
            );
            TrustedProxiesConfig::empty()
        }
    }
}

/// Parse `APP_TRUSTED_PROXIES` strict — bad entries abort boot. Used
/// by `try_from_env` (`Config::init` calls this).
fn parse_trusted_proxies_strict() -> Result<TrustedProxiesConfig, FrameworkError> {
    let Ok(raw) = std::env::var("APP_TRUSTED_PROXIES") else {
        return Ok(TrustedProxiesConfig::empty());
    };
    let ips = parse_ip_list(&raw).map_err(|bad| {
        FrameworkError::internal(format!(
            "APP_TRUSTED_PROXIES contains an unparseable IP address: {bad:?}"
        ))
    })?;
    Ok(TrustedProxiesConfig::with_ips(ips))
}

fn parse_ip_list(raw: &str) -> Result<Vec<IpAddr>, String> {
    let mut out = Vec::new();
    for entry in raw.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed.parse::<IpAddr>() {
            Ok(ip) => out.push(ip),
            Err(_) => return Err(trimmed.to_string()),
        }
    }
    Ok(out)
}

/// Builder for AppConfig
#[derive(Default)]
pub struct AppConfigBuilder {
    name: Option<String>,
    environment: Option<Environment>,
    debug: Option<bool>,
    url: Option<String>,
    trusted_proxies: Option<TrustedProxiesConfig>,
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

    /// Set the trusted-proxies allowlist that gates
    /// [`Request::ip`](crate::Request::ip) and the related accessors.
    pub fn trusted_proxies(mut self, cfg: TrustedProxiesConfig) -> Self {
        self.trusted_proxies = Some(cfg);
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
            trusted_proxies: self.trusted_proxies.unwrap_or(default.trusted_proxies),
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

    #[test]
    fn parse_ip_list_returns_each_address() {
        let ips = parse_ip_list("127.0.0.1, 10.0.0.1, ::1").expect("parse");
        assert_eq!(ips.len(), 3);
    }

    #[test]
    fn parse_ip_list_skips_empty_entries() {
        let ips = parse_ip_list(",,127.0.0.1, , 10.0.0.1,").expect("parse");
        assert_eq!(ips.len(), 2);
    }

    #[test]
    fn parse_ip_list_returns_bad_entry_on_error() {
        let err = parse_ip_list("127.0.0.1, not-an-ip").expect_err("bad entry");
        assert_eq!(err, "not-an-ip");
    }

    #[test]
    #[serial_test::serial(app_config_env)]
    fn try_from_env_rejects_unparseable_trusted_proxy() {
        let prior = std::env::var("APP_TRUSTED_PROXIES").ok();
        // SAFETY: single-threaded mutation of a process-global env var
        // gated by the `app_config_env` serial token (shared with the
        // sibling tests in this module). We restore the prior value at
        // the end.
        unsafe {
            std::env::set_var("APP_TRUSTED_PROXIES", "127.0.0.1, not-an-ip");
        }
        let err = AppConfig::try_from_env().expect_err("bad IP must error");
        let msg = format!("{}", err);
        assert!(
            msg.contains("APP_TRUSTED_PROXIES"),
            "error should name the env var: {:?}",
            msg
        );
        assert!(
            msg.contains("not-an-ip"),
            "error should quote the bad entry: {:?}",
            msg
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("APP_TRUSTED_PROXIES", v),
                None => std::env::remove_var("APP_TRUSTED_PROXIES"),
            }
        }
    }
}
