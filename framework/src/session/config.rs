//! Session configuration

use std::time::Duration;

/// Session configuration
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Session lifetime
    pub lifetime: Duration,
    /// Cookie name for the session ID
    pub cookie_name: String,
    /// Cookie path
    pub cookie_path: String,
    /// Whether to set Secure flag on cookie (HTTPS only)
    pub cookie_secure: bool,
    /// Whether to set HttpOnly flag on cookie
    pub cookie_http_only: bool,
    /// SameSite attribute for the cookie
    pub cookie_same_site: String,
    /// Database table name for sessions
    pub table_name: String,
    /// Lifetime of a remember-me token (and its cookie). Default 30 days.
    ///
    /// Configured separately from `lifetime` because remember-me is the
    /// "I closed my browser and want to come back next month" path —
    /// the session cookie itself is short-lived (default 2 hours).
    pub remember_lifetime: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            lifetime: Duration::from_secs(120 * 60), // 2 hours (120 minutes)
            cookie_name: "suprnova_session".to_string(),
            cookie_path: "/".to_string(),
            cookie_secure: true,
            cookie_http_only: true,
            cookie_same_site: "Lax".to_string(),
            table_name: "sessions".to_string(),
            remember_lifetime: Duration::from_secs(30 * 24 * 60 * 60), // 30 days
        }
    }
}

impl SessionConfig {
    /// Create a new session config with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Load session configuration from environment variables
    ///
    /// Environment variables:
    /// - `SESSION_LIFETIME`: Session lifetime in minutes (default: 120)
    /// - `SESSION_COOKIE`: Cookie name (default: suprnova_session)
    /// - `SESSION_SECURE`: Set Secure flag (default: true)
    /// - `SESSION_PATH`: Cookie path (default: /)
    /// - `SESSION_SAME_SITE`: SameSite attribute (default: Lax)
    /// - `REMEMBER_LIFETIME`: Remember-me token/cookie lifetime in minutes (default: 43200 = 30 days)
    pub fn from_env() -> Self {
        let lifetime_minutes: u64 = crate::env_optional("SESSION_LIFETIME")
            .and_then(|s: String| s.parse().ok())
            .unwrap_or(120);

        let cookie_secure = crate::env_optional("SESSION_SECURE")
            .map(|s: String| s.to_lowercase() == "true" || s == "1")
            .unwrap_or(true);

        let remember_lifetime_minutes: u64 = crate::env_optional("REMEMBER_LIFETIME")
            .and_then(|s: String| s.parse().ok())
            .unwrap_or(30 * 24 * 60); // 30 days

        Self {
            lifetime: Duration::from_secs(lifetime_minutes * 60),
            cookie_name: crate::env_optional("SESSION_COOKIE")
                .unwrap_or_else(|| "suprnova_session".to_string()),
            cookie_path: crate::env_optional("SESSION_PATH").unwrap_or_else(|| "/".to_string()),
            cookie_secure,
            cookie_http_only: true, // Always true for security
            cookie_same_site: crate::env_optional("SESSION_SAME_SITE")
                .unwrap_or_else(|| "Lax".to_string()),
            table_name: "sessions".to_string(),
            remember_lifetime: Duration::from_secs(remember_lifetime_minutes * 60),
        }
    }

    /// Set the session lifetime
    pub fn lifetime(mut self, duration: Duration) -> Self {
        self.lifetime = duration;
        self
    }

    /// Set the cookie name
    pub fn cookie_name(mut self, name: impl Into<String>) -> Self {
        self.cookie_name = name.into();
        self
    }

    /// Set whether the cookie should be secure (HTTPS only)
    pub fn secure(mut self, secure: bool) -> Self {
        self.cookie_secure = secure;
        self
    }

    /// Set the remember-me token/cookie lifetime.
    pub fn remember_lifetime(mut self, duration: Duration) -> Self {
        self.remember_lifetime = duration;
        self
    }
}
