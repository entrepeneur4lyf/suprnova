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

/// Load environment variables from `.env` files with proper precedence.
///
/// Precedence (highest wins):
/// 1. Actual system environment variables (highest)
/// 2. `.env.{environment}.local` (environment-specific local overrides)
/// 3. `.env.{environment}` (environment-specific)
/// 4. `.env.local` (local overrides, not committed)
/// 5. `.env` (base defaults — lowest)
///
/// The loader works in two phases so that `APP_ENV` defined in the base
/// `.env` correctly selects environment-specific files for the same
/// invocation (codex review finding #12):
///
/// 1. Snapshot every key that was already present in the system
///    environment when the function was called. These wins over every
///    file value, full stop.
/// 2. Load base `.env` (non-overriding — file values fill in gaps in
///    the system env).
/// 3. Re-detect `APP_ENV` now that base `.env` has been merged.
/// 4. Load `.env.local`, `.env.{env}`, `.env.{env}.local` in
///    least-to-most-specific order using `from_path_override` so each
///    later file wins over earlier files.
/// 5. Re-apply the system-env snapshot last so real system values
///    survive any file that tried to override them.
pub fn load_dotenv(project_root: &Path) -> Environment {
    // Phase 1: snapshot real system env so we can restore precedence at
    // the end. Captures only keys present BEFORE any file load.
    let system_env: Vec<(String, String)> = std::env::vars().collect();

    // Phase 2: load base `.env` non-overriding. Anything already in
    // system env (i.e. the snapshot) stays untouched.
    let _ = dotenvy::from_path(project_root.join(".env"));

    // Phase 3: re-detect APP_ENV now that base `.env` has merged in.
    // This is the fix for finding #12 — previously APP_ENV was
    // detected before `.env` loaded, so `APP_ENV=production` set only
    // in `.env` never selected `.env.production`.
    let env = Environment::detect();

    // Phase 4: load environment-specific files in least-to-most-
    // specific order, using `from_path_override` so each later file
    // beats the earlier file. We do NOT want these to override real
    // system env — we restore that in phase 5.
    let _ = dotenvy::from_path_override(project_root.join(".env.local"));

    if let Some(suffix) = env.env_file_suffix() {
        let path = project_root.join(format!(".env.{}", suffix));
        let _ = dotenvy::from_path_override(&path);

        let path = project_root.join(format!(".env.{}.local", suffix));
        let _ = dotenvy::from_path_override(&path);
    }

    // Phase 5: restore real system env. Any key that existed in the
    // process environment BEFORE this function ran is rewritten back to
    // its original value, defeating anything a file tried to override.
    //
    // SAFETY: `std::env::set_var` is process-global; documented unsafe
    // because it races with concurrent getenv on some platforms. We're
    // in the boot path before workers start, and callers serialize
    // `load_dotenv` (it is meant to be called once at startup).
    for (k, v) in system_env {
        unsafe {
            std::env::set_var(&k, &v);
        }
    }

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
