use std::path::Path;

/// Environment type enumeration
#[derive(Debug, Clone, PartialEq)]
pub enum Environment {
    Local,
    Development,
    Staging,
    Production,
    Testing,
    Custom(String),
}

impl Environment {
    /// Detect environment from APP_ENV or default to Local
    pub fn detect() -> Self {
        match std::env::var("APP_ENV").ok().as_deref() {
            Some("production") => Self::Production,
            Some("staging") => Self::Staging,
            Some("development") => Self::Development,
            Some("testing") => Self::Testing,
            Some("local") | None => Self::Local,
            Some(other) => Self::Custom(other.to_string()),
        }
    }

    /// Get the .env file suffix for this environment
    pub fn env_file_suffix(&self) -> Option<&str> {
        match self {
            Self::Local => Some("local"),
            Self::Production => Some("production"),
            Self::Staging => Some("staging"),
            Self::Development => Some("development"),
            Self::Testing => Some("testing"),
            Self::Custom(name) => Some(name.as_str()),
        }
    }

    /// Check if this is a production environment
    pub fn is_production(&self) -> bool {
        matches!(self, Self::Production)
    }

    /// Check if this is a development environment (local or development)
    pub fn is_development(&self) -> bool {
        matches!(self, Self::Local | Self::Development)
    }
}

impl std::fmt::Display for Environment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Development => write!(f, "development"),
            Self::Staging => write!(f, "staging"),
            Self::Production => write!(f, "production"),
            Self::Testing => write!(f, "testing"),
            Self::Custom(name) => write!(f, "{}", name),
        }
    }
}

/// Load environment variables from .env files with proper precedence
///
/// Precedence (later files override earlier):
/// 1. .env (base defaults)
/// 2. .env.local (local overrides, not committed)
/// 3. .env.{environment} (environment-specific)
/// 4. .env.{environment}.local (environment-specific local overrides)
/// 5. Actual system environment variables (highest priority)
pub fn load_dotenv(project_root: &Path) -> Environment {
    let env = Environment::detect();

    // Load in REVERSE order of precedence because dotenvy doesn't overwrite existing vars
    // So we load most specific first, then less specific files won't override

    // 4. Environment-specific local (e.g., .env.production.local) - highest file priority
    if let Some(suffix) = env.env_file_suffix() {
        let path = project_root.join(format!(".env.{}.local", suffix));
        let _ = dotenvy::from_path(&path);
    }

    // 3. Environment-specific (e.g., .env.production)
    if let Some(suffix) = env.env_file_suffix() {
        let path = project_root.join(format!(".env.{}", suffix));
        let _ = dotenvy::from_path(&path);
    }

    // 2. .env.local
    let _ = dotenvy::from_path(project_root.join(".env.local"));

    // 1. .env (base) - lowest file priority
    let _ = dotenvy::from_path(project_root.join(".env"));

    env
}

/// Get an environment variable with a default value
///
/// # Example
/// ```
/// use suprnova::config::env;
///
/// let port: u16 = env("SERVER_PORT", 8080);
/// let host = env("SERVER_HOST", "127.0.0.1".to_string());
/// ```
pub fn env<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Get a required environment variable (panics if not set or invalid)
///
/// # Panics
/// Panics if the environment variable is not set or cannot be parsed
///
/// # Example
/// ```no_run
/// use suprnova::config::env_required;
///
/// let secret: String = env_required("APP_SECRET");
/// ```
pub fn env_required<T: std::str::FromStr>(key: &str) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            panic!(
                "Required environment variable {} is not set or invalid",
                key
            )
        })
}

/// Get an optional environment variable
///
/// # Example
/// ```
/// use suprnova::config::env_optional;
///
/// let debug: Option<bool> = env_optional("APP_DEBUG");
/// ```
pub fn env_optional<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok().and_then(|v| v.parse().ok())
}
