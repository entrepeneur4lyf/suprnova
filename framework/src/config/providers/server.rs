use crate::config::env::env;
use crate::http::body::DEFAULT_MAX_REQUEST_BODY_BYTES;

/// Server configuration.
///
/// `max_body_size` is honoured: `Server::from_config` calls
/// [`crate::http::body::set_global_max_request_body_bytes`] with this
/// value during boot, so `SERVER_MAX_BODY_SIZE=...` in the env actually
/// changes the request body cap. Per-`FormRequest::max_body_bytes`
/// overrides still take precedence on individual endpoints.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Server host address.
    pub host: String,
    /// Server port.
    pub port: u16,
    /// Maximum request body size in bytes.
    ///
    /// Defaults to [`DEFAULT_MAX_REQUEST_BODY_BYTES`] (8 MiB). Override
    /// via `SERVER_MAX_BODY_SIZE` in the environment. The configured
    /// value is wired into the process-global body cap at boot time;
    /// per-FormRequest `max_body_bytes` overrides still apply on
    /// individual endpoints.
    pub max_body_size: usize,
}

impl ServerConfig {
    /// Build config from environment variables. The default for
    /// `max_body_size` is [`DEFAULT_MAX_REQUEST_BODY_BYTES`] so the
    /// "no env var set" case matches the compile-time fallback used
    /// by [`crate::http::body::global_max_request_body_bytes`] before
    /// boot wires the runtime value in.
    pub fn from_env() -> Self {
        Self {
            host: env("SERVER_HOST", "127.0.0.1".to_string()),
            port: env("SERVER_PORT", 8080),
            max_body_size: env("SERVER_MAX_BODY_SIZE", DEFAULT_MAX_REQUEST_BODY_BYTES),
        }
    }

    /// Create a builder for customizing config
    pub fn builder() -> ServerConfigBuilder {
        ServerConfigBuilder::default()
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

/// Builder for ServerConfig
#[derive(Default)]
pub struct ServerConfigBuilder {
    host: Option<String>,
    port: Option<u16>,
    max_body_size: Option<usize>,
}

impl ServerConfigBuilder {
    /// Set the server host
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    /// Set the server port
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Set the maximum request body size in bytes
    pub fn max_body_size(mut self, size: usize) -> Self {
        self.max_body_size = Some(size);
        self
    }

    /// Build the ServerConfig
    pub fn build(self) -> ServerConfig {
        let default = ServerConfig::from_env();
        ServerConfig {
            host: self.host.unwrap_or(default.host),
            port: self.port.unwrap_or(default.port),
            max_body_size: self.max_body_size.unwrap_or(default.max_body_size),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Domain 4 audit fix C1 regression: ServerConfig::from_env's default
    //! for `max_body_size` must match the body-collector's compile-time
    //! default so a missing env var doesn't silently change the cap.
    //!
    //! Note: we don't assert on env-var-driven values here because tests
    //! share a process env and `SERVER_MAX_BODY_SIZE` could leak in from
    //! another test. The default-alignment invariant is what matters for
    //! the audit regression.

    use super::*;

    #[test]
    fn default_max_body_size_matches_body_module_default() {
        // Unset the var so from_env hits its default path. Use a unique
        // scope guard so we don't disturb other tests on the same
        // process: stash the prior value, clear, run assertion, restore.
        let prior = std::env::var("SERVER_MAX_BODY_SIZE").ok();
        // SAFETY: tests run single-threaded for this scope only because
        // we don't await across the modification; module-level config
        // env-var tests in the rest of the crate use the same pattern.
        unsafe {
            std::env::remove_var("SERVER_MAX_BODY_SIZE");
        }
        let config = ServerConfig::from_env();
        assert_eq!(
            config.max_body_size, DEFAULT_MAX_REQUEST_BODY_BYTES,
            "ServerConfig default must match the body collector's \
             DEFAULT_MAX_REQUEST_BODY_BYTES — divergent defaults caused \
             SERVER_MAX_BODY_SIZE to be a dead knob"
        );
        // Restore prior env state for sibling tests.
        if let Some(v) = prior {
            unsafe {
                std::env::set_var("SERVER_MAX_BODY_SIZE", v);
            }
        }
    }
}
