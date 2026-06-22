use crate::config::env::{env, env_optional, env_strict};
use crate::error::FrameworkError;
use crate::http::body::DEFAULT_MAX_REQUEST_BODY_BYTES;

/// Default listen port when neither `SERVER_PORT` nor `PORT` is set.
///
/// Chosen to be distinctive — `8080`/`8000` collide with nearly every
/// other dev server and proxy on a typical machine. `8765` (an 8-7-6-5
/// countdown) is rarely squatted; the `8xxx` prefix keeps it readable as
/// a backend port. The matching Vite default is `5765`
/// ([`crate::inertia::config::DEFAULT_VITE_PORT`]).
pub const DEFAULT_SERVER_PORT: u16 = 8765;

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
    /// Optional cap on the number of concurrently active TCP connections.
    ///
    /// When `Some(n)`, the server acquires a semaphore permit for each
    /// accepted connection and holds it until the connection closes;
    /// once all `n` permits are taken the accept loop blocks until an
    /// existing connection ends. When `None` (the default), behaviour is
    /// unchanged — connections are unbounded.
    ///
    /// Set via `SERVER_MAX_CONNECTIONS` in the environment. Blank,
    /// unparseable, or zero values are treated as `None` so a typo does
    /// not prevent the server from starting. This is a backstop against
    /// runaway connection accumulation; pair it with a reverse proxy and
    /// an appropriate `LimitNOFILE` for full protection.
    pub max_connections: Option<usize>,
}

impl ServerConfig {
    /// Build config from environment variables. The default for
    /// `max_body_size` is [`DEFAULT_MAX_REQUEST_BODY_BYTES`] so the
    /// "no env var set" case matches the compile-time fallback used
    /// by [`crate::http::body::global_max_request_body_bytes`] before
    /// boot wires the runtime value in.
    ///
    /// This helper is intentionally lenient — a typed env var that
    /// fails to parse falls back to the default (with a
    /// `tracing::warn!`). It is invoked from `impl Default` and other
    /// infallible paths. The strict, boot-failing variant is
    /// [`Self::try_from_env`]; `Config::init` calls that.
    pub fn from_env() -> Self {
        Self {
            host: env("SERVER_HOST", "127.0.0.1".to_string()),
            port: resolve_port_lenient(),
            max_body_size: env("SERVER_MAX_BODY_SIZE", DEFAULT_MAX_REQUEST_BODY_BYTES),
            max_connections: parse_max_connections(
                std::env::var("SERVER_MAX_CONNECTIONS").ok().as_deref(),
            ),
        }
    }

    /// Build config from environment variables, returning an error if
    /// any typed knob is set to a value that fails to parse. Used by
    /// `Config::init` so a typo in `SERVER_PORT` aborts boot instead
    /// of silently reverting to the default.
    pub fn try_from_env() -> Result<Self, FrameworkError> {
        let host = env_strict::<String>("SERVER_HOST")?.unwrap_or_else(|| "127.0.0.1".to_string());
        // Precedence: SERVER_PORT (explicit) > PORT (PaaS convention —
        // Heroku/Railway/Render/Fly inject it) > distinctive default.
        let port = match env_strict::<u16>("SERVER_PORT")? {
            Some(p) => p,
            None => env_strict::<u16>("PORT")?.unwrap_or(DEFAULT_SERVER_PORT),
        };
        let max_body_size =
            env_strict::<usize>("SERVER_MAX_BODY_SIZE")?.unwrap_or(DEFAULT_MAX_REQUEST_BODY_BYTES);
        // `SERVER_MAX_CONNECTIONS` is intentionally lenient even in the
        // strict path: a typo here should not abort boot, just log via
        // parse_max_connections returning None.
        let max_connections =
            parse_max_connections(std::env::var("SERVER_MAX_CONNECTIONS").ok().as_deref());
        Ok(Self {
            host,
            port,
            max_body_size,
            max_connections,
        })
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

/// Resolve the listen port for the lenient (non-boot) path.
///
/// Precedence mirrors [`ServerConfig::try_from_env`]: `SERVER_PORT` >
/// `PORT` (PaaS convention) > [`DEFAULT_SERVER_PORT`]. Unparseable values
/// are treated as unset here (the strict `try_from_env` path is what
/// fails boot on a typo).
fn resolve_port_lenient() -> u16 {
    env_optional::<u16>("SERVER_PORT")
        .or_else(|| env_optional::<u16>("PORT"))
        .unwrap_or(DEFAULT_SERVER_PORT)
}

/// Parse the optional `SERVER_MAX_CONNECTIONS` cap from a raw env-var
/// string. A blank or unparseable value — or zero — is treated as unset
/// (`None` = unbounded) rather than a boot error: it is an optional
/// hardening knob, not a required setting, and a typo should not prevent
/// the server from starting.
pub(crate) fn parse_max_connections(raw: Option<&str>) -> Option<usize> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
}

/// Builder for ServerConfig
#[derive(Default)]
pub struct ServerConfigBuilder {
    host: Option<String>,
    port: Option<u16>,
    max_body_size: Option<usize>,
    max_connections: Option<usize>,
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

    /// Cap the number of concurrently active connections.
    ///
    /// When set, the server will not accept new connections once this
    /// many are active; the accept loop blocks until an existing
    /// connection closes. When unset, connections are unbounded.
    pub fn max_connections(mut self, n: usize) -> Self {
        self.max_connections = Some(n);
        self
    }

    /// Build the ServerConfig
    pub fn build(self) -> ServerConfig {
        let default = ServerConfig::from_env();
        ServerConfig {
            host: self.host.unwrap_or(default.host),
            port: self.port.unwrap_or(default.port),
            max_body_size: self.max_body_size.unwrap_or(default.max_body_size),
            max_connections: self.max_connections.or(default.max_connections),
        }
    }
}

#[cfg(test)]
mod tests {
    //! `ServerConfig::from_env`'s default for `max_body_size` must
    //! match the body-collector's compile-time default so a missing
    //! env var doesn't silently change the cap.
    //!
    //! Note: we don't assert on env-var-driven values here because tests
    //! share a process env and `SERVER_MAX_BODY_SIZE` could leak in from
    //! another test. The default-alignment invariant is what matters for
    //! the audit regression.

    use super::*;

    #[test]
    fn parses_max_connections_from_env() {
        // unset → None (unbounded, behavior unchanged)
        assert_eq!(parse_max_connections(None), None);
        // set → Some(n)
        assert_eq!(parse_max_connections(Some("1024")), Some(1024));
        // blank / unparseable → None (don't fail boot over a typo'd optional knob)
        assert_eq!(parse_max_connections(Some("")), None);
        assert_eq!(parse_max_connections(Some("abc")), None);
        // zero → None (a zero cap would be a dead lock; treat as unset)
        assert_eq!(parse_max_connections(Some("0")), None);
        // whitespace-padded value is accepted
        assert_eq!(parse_max_connections(Some("  512  ")), Some(512));
    }

    #[test]
    #[serial_test::serial(server_config_env)]
    fn try_from_env_rejects_unparseable_port() {
        // `SERVER_PORT=abc` must fail boot via `try_from_env`, not
        // silently fall back to the default the way the lenient
        // `from_env` path does. This is the boot-time fail-loud
        // guarantee `Config::init` relies on.
        let prior = std::env::var("SERVER_PORT").ok();
        // SAFETY: this test mutates a process-global env var. Other
        // tests in this crate use the same single-threaded pattern;
        // we restore the prior value at the end.
        unsafe {
            std::env::set_var("SERVER_PORT", "abc");
        }
        let err = ServerConfig::try_from_env().expect_err("unparseable port must error");
        let msg = format!("{}", err);
        assert!(
            msg.contains("SERVER_PORT"),
            "error should name the env var: {:?}",
            msg
        );
        unsafe {
            match prior {
                Some(v) => std::env::set_var("SERVER_PORT", v),
                None => std::env::remove_var("SERVER_PORT"),
            }
        }
    }

    #[test]
    #[serial_test::serial(server_config_env)]
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

    #[test]
    #[serial_test::serial(server_config_env)]
    fn port_precedence_server_port_then_port_then_default() {
        let prior_server = std::env::var("SERVER_PORT").ok();
        let prior_port = std::env::var("PORT").ok();

        // SAFETY: single-threaded scope (no await across the mutation),
        // restored at the end — same pattern as the sibling env tests.
        unsafe {
            std::env::remove_var("SERVER_PORT");
            std::env::remove_var("PORT");
        }

        // Neither set → distinctive default.
        assert_eq!(ServerConfig::from_env().port, DEFAULT_SERVER_PORT);
        assert_eq!(
            ServerConfig::try_from_env().unwrap().port,
            DEFAULT_SERVER_PORT
        );

        // Only PORT set (PaaS: Heroku/Railway/Render/Fly inject it).
        unsafe {
            std::env::set_var("PORT", "4321");
        }
        assert_eq!(ServerConfig::from_env().port, 4321);
        assert_eq!(ServerConfig::try_from_env().unwrap().port, 4321);

        // SERVER_PORT wins over PORT when both are set.
        unsafe {
            std::env::set_var("SERVER_PORT", "9999");
        }
        assert_eq!(ServerConfig::from_env().port, 9999);
        assert_eq!(ServerConfig::try_from_env().unwrap().port, 9999);

        unsafe {
            match prior_server {
                Some(v) => std::env::set_var("SERVER_PORT", v),
                None => std::env::remove_var("SERVER_PORT"),
            }
            match prior_port {
                Some(v) => std::env::set_var("PORT", v),
                None => std::env::remove_var("PORT"),
            }
        }
    }
}
