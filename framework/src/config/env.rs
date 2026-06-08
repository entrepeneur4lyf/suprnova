//! `.env` file loading and environment-detection helpers.
//!
//! Layered semantics: real OS env vars always win over file-loaded
//! values; on repeat calls the loader removes stale file values before
//! re-reading, so a test harness or hot-reload loop never promotes an
//! older `.env` value back into the "real" tier.

use std::path::Path;
use std::sync::Mutex;

use crate::error::FrameworkError;

/// (Key, value) pairs that a previous `load_dotenv` call introduced
/// into the process environment. Tracked so repeat calls (e.g. test
/// binaries that load multiple project roots, or hot-reload paths)
/// don't accidentally promote a stale file value from an earlier root
/// into the "real system env" tier on the next call.
///
/// The mechanism: at the top of `load_dotenv` we remove every tracked
/// key whose current value still matches the value the loader left
/// behind — that's the "leftover" case. Keys whose values have changed
/// since the previous call survive: someone (the OS, a test harness,
/// or the application itself) explicitly set them, so they win like
/// every other real system env var.
///
/// Real OS-provided env vars are never touched because the map only
/// ever contains keys-and-values that load_dotenv itself wrote.
static LOADED_KEYS: Mutex<Option<std::collections::HashMap<String, String>>> = Mutex::new(None);

/// Environment type enumeration
#[derive(Debug, Clone, PartialEq)]
pub enum Environment {
    /// Local developer machine (`APP_ENV=local`). The default when `APP_ENV` is unset.
    Local,
    /// Development / staging-adjacent environment for the developer's own services (`APP_ENV=development|dev`).
    Development,
    /// Pre-production staging environment (`APP_ENV=staging|stage`).
    Staging,
    /// Live production environment (`APP_ENV=production|prod`). Enables the strictest safety defaults.
    Production,
    /// Automated-test environment (`APP_ENV=testing|test`). Recognized by the test harness.
    Testing,
    /// Any unrecognized `APP_ENV` value, preserved verbatim.
    Custom(String),
}

impl Environment {
    /// Detect environment from `APP_ENV` or default to `Local`.
    ///
    /// Matching is case-insensitive and accepts a small set of common
    /// aliases so an operator's `APP_ENV=Production`, `APP_ENV=PROD`,
    /// or `APP_ENV=prod` is recognized as `Production` instead of
    /// silently falling through to `Custom(...)` (which makes
    /// `is_production()` return `false` and bypasses production-only
    /// behavior gated on it). Laravel does an exact case-sensitive
    /// match on `APP_ENV`; we diverge to fail-safe on the common
    /// casing/alias mistakes Rust deployments hit in the wild.
    ///
    /// Recognized aliases:
    /// - `prod` → [`Self::Production`]
    /// - `dev` → [`Self::Development`]
    /// - `stage`, `stg` → [`Self::Staging`]
    /// - `test` → [`Self::Testing`]
    ///
    /// Values that don't match a known variant or alias are stored in
    /// [`Self::Custom`] with their original casing preserved — that
    /// string flows back through [`Self::env_file_suffix`] to pick the
    /// `.env.<suffix>` file, and lowercasing it here would silently
    /// change which file loads for a real custom environment
    /// (e.g. `APP_ENV=QA` must continue to load `.env.QA`).
    pub fn detect() -> Self {
        let raw = match std::env::var("APP_ENV").ok() {
            None => return Self::Local,
            Some(s) => s,
        };
        match raw.to_lowercase().as_str() {
            "production" | "prod" => Self::Production,
            "staging" | "stage" | "stg" => Self::Staging,
            "development" | "dev" => Self::Development,
            "testing" | "test" => Self::Testing,
            "local" => Self::Local,
            _ => Self::Custom(raw),
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
/// The loader runs five phases so that `APP_ENV` defined in the base
/// `.env` correctly selects environment-specific files for the same
/// invocation:
///
/// 1. Strip every key that a previous `load_dotenv` call introduced
///    into the process env, then snapshot the remaining keys as the
///    "real system env" tier. Real OS-provided variables are never
///    touched because the tracked set only contains keys we added.
///    This keeps repeated calls (e.g. different project roots in the
///    same test binary) idempotent: stale file values from an earlier
///    root never get promoted to system-tier precedence on the next
///    call.
/// 2. Load base `.env` (non-overriding — file values fill in gaps in
///    the system env).
/// 3. Re-detect `APP_ENV` now that base `.env` has been merged.
/// 4. Load `.env.local`, `.env.{env}`, `.env.{env}.local` in
///    least-to-most-specific order using `from_path_override` so each
///    later file wins over earlier files.
/// 5. Re-apply the system-env snapshot last so real system values
///    survive any file that tried to override them, and record every
///    key newly introduced for the next-call cleanup.
///
/// # Errors
///
/// Returns [`FrameworkError::Internal`] when any candidate `.env` file
/// exists but cannot be read or parsed (IO errors, malformed lines).
/// Missing `.env` files are NOT an error — they are an expected case
/// for environments where configuration is fully supplied by the
/// process environment.
pub fn load_dotenv(project_root: &Path) -> Result<Environment, FrameworkError> {
    // Phase 1a: strip keys previously introduced by load_dotenv so the
    // upcoming snapshot reflects only the real system env. Without
    // this, a second call would treat the prior call's file values as
    // "system env" and freeze them in place.
    //
    // SAFETY: `std::env::remove_var` is process-global; documented
    // unsafe because it races with concurrent getenv on some
    // platforms. We're in the boot path before workers start, and
    // callers serialize `load_dotenv` (it is meant to be called once
    // at startup).
    let guard = LOADED_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(prev) = guard.as_ref() {
        for (k, expected) in prev {
            // Only strip the leftover if the env still holds the exact
            // value load_dotenv left there. If something else has since
            // overwritten it (the test harness, the OS, the app), treat
            // that as a real system value and leave it alone.
            if matches!(std::env::var(k).ok(), Some(current) if current == *expected) {
                unsafe {
                    std::env::remove_var(k);
                }
            }
        }
    }
    drop(guard);

    // Phase 1b: snapshot real system env so we can restore precedence
    // at the end. Captures only keys present BEFORE any file load.
    let system_env: Vec<(String, String)> = std::env::vars().collect();
    let system_keys: std::collections::HashSet<String> =
        system_env.iter().map(|(k, _)| k.clone()).collect();

    // Phase 2: load base `.env` non-overriding. Anything already in
    // system env (i.e. the snapshot) stays untouched. Distinguish
    // "file missing" (OK) from "IO/parse error" (boot failure).
    load_env_file(&project_root.join(".env"), false)?;

    // Phase 3: re-detect APP_ENV now that base `.env` has merged in.
    // Detecting before the base load would skip `.env.production`
    // when `APP_ENV=production` was set only in `.env`.
    let env = Environment::detect();

    // Phase 4: load environment-specific files in least-to-most-
    // specific order, using `from_path_override` so each later file
    // beats the earlier file. We do NOT want these to override real
    // system env — we restore that in phase 5.
    load_env_file(&project_root.join(".env.local"), true)?;

    if let Some(suffix) = env.env_file_suffix() {
        let path = project_root.join(format!(".env.{}", suffix));
        load_env_file(&path, true)?;

        let path = project_root.join(format!(".env.{}.local", suffix));
        load_env_file(&path, true)?;
    }

    // Phase 5a: restore real system env. Any key that existed in the
    // process environment BEFORE this function ran is rewritten back to
    // its original value, defeating anything a file tried to override.
    //
    // SAFETY: see Phase 1a safety note — same boot-time invariant.
    for (k, v) in &system_env {
        unsafe {
            std::env::set_var(k, v);
        }
    }

    // Phase 5b: record the (key, value) pairs *introduced* by this
    // call (in the current env minus the system-env snapshot) so a
    // follow-up call can strip them in Phase 1a — but only if their
    // value still matches what we left, see Phase 1a comment.
    let mut introduced = std::collections::HashMap::new();
    for (k, v) in std::env::vars() {
        if !system_keys.contains(&k) {
            introduced.insert(k, v);
        }
    }
    let mut guard = LOADED_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(introduced);

    Ok(env)
}

/// Load a single `.env`-style file. `override_existing=false` matches
/// `dotenvy::from_path` semantics (only fill missing keys);
/// `override_existing=true` matches `dotenvy::from_path_override`.
///
/// File-not-found is treated as success — that's the expected case
/// for optional layers (`.env.local`, `.env.production`, etc.).
/// Every other IO or parse failure becomes a [`FrameworkError`] so
/// boot fails loudly on a typo in `.env.production`.
fn load_env_file(path: &Path, override_existing: bool) -> Result<(), FrameworkError> {
    let result = if override_existing {
        dotenvy::from_path_override(path)
    } else {
        dotenvy::from_path(path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.not_found() => Ok(()),
        Err(e) => Err(FrameworkError::Internal {
            message: format!("failed to load env file {}: {}", path.display(), e),
        }),
    }
}

/// Get an environment variable with a default value
///
/// Returns the parsed value when the variable is set and parses
/// cleanly, or `default` otherwise. When the variable is set but
/// fails to parse the call emits `tracing::warn!` so a typo in a
/// production env doesn't disappear silently — but the call itself
/// remains infallible because this is a Laravel-parity helper used
/// from a wide variety of call sites (including `impl Default`).
/// Strict validation of typed framework knobs lives in
/// [`crate::config::providers::ServerConfig::try_from_env`] and
/// [`crate::config::providers::AppConfig::try_from_env`].
///
/// # Example
/// ```
/// use suprnova::config::env;
///
/// let port: u16 = env("SERVER_PORT", 8080);
/// let host = env("SERVER_HOST", "127.0.0.1".to_string());
/// ```
pub fn env<T: std::str::FromStr>(key: &str, default: T) -> T {
    match std::env::var(key) {
        Ok(raw) => match raw.parse() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    env_var = key,
                    raw_value = %raw,
                    "environment variable is set but failed to parse; falling back to default"
                );
                default
            }
        },
        Err(_) => default,
    }
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
/// Returns `Some(value)` when the variable is set and parses cleanly,
/// `None` when the variable is unset, and `None` with a `tracing::warn!`
/// when the variable is set but unparseable. The warning is so a typo
/// doesn't disappear silently; strict typed validation lives in the
/// `try_from_env` helpers on the typed config structs.
///
/// # Example
/// ```
/// use suprnova::config::env_optional;
///
/// let debug: Option<bool> = env_optional("APP_DEBUG");
/// ```
pub fn env_optional<T: std::str::FromStr>(key: &str) -> Option<T> {
    match std::env::var(key) {
        Ok(raw) => match raw.parse() {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!(
                    env_var = key,
                    raw_value = %raw,
                    "optional environment variable is set but failed to parse; treating as unset"
                );
                None
            }
        },
        Err(_) => None,
    }
}

/// Test-only: clear the [`LOADED_KEYS`] tracker so a fresh
/// `load_dotenv` call behaves as if it were the first invocation
/// for this process. Used by tests that exercise system-env-wins
/// semantics with a value that happens to coincide with whatever
/// a sibling test left in the tracker — the value-matching strip
/// in Phase 1a cannot distinguish "leftover" from "user re-set to
/// the same string".
///
/// Hidden from docs; this is not part of the public API.
#[doc(hidden)]
pub fn __reset_loaded_keys_for_tests() {
    let mut guard = LOADED_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    *guard = None;
}

/// Try to parse an environment variable into `T`, failing loudly when
/// the variable is set but unparseable. Returns `Ok(None)` when unset.
///
/// Used by the strict `try_from_env` helpers on typed config structs
/// so that `Config::init` surfaces typos at boot time instead of
/// silently falling back to defaults.
pub(crate) fn env_strict<T: std::str::FromStr>(key: &str) -> Result<Option<T>, FrameworkError> {
    match std::env::var(key) {
        Ok(raw) => match raw.parse() {
            Ok(v) => Ok(Some(v)),
            Err(_) => Err(FrameworkError::Internal {
                message: format!(
                    "environment variable {} is set but could not be parsed as {}: {:?}",
                    key,
                    std::any::type_name::<T>(),
                    raw
                ),
            }),
        },
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Save the current `APP_ENV` value, set it to `value`, run `f`, then
    /// restore the prior state. Tests calling this must be `#[serial]`
    /// because `APP_ENV` is process-global.
    fn with_app_env(value: Option<&str>, f: impl FnOnce()) {
        let prior = std::env::var("APP_ENV").ok();
        // SAFETY: mutates a process-global env var. Callers serialize via
        // `serial_test` so this never races a sibling test.
        unsafe {
            match value {
                Some(v) => std::env::set_var("APP_ENV", v),
                None => std::env::remove_var("APP_ENV"),
            }
        }
        f();
        unsafe {
            match prior {
                Some(v) => std::env::set_var("APP_ENV", v),
                None => std::env::remove_var("APP_ENV"),
            }
        }
    }

    #[test]
    #[serial_test::serial(app_config_env)]
    fn detect_recognizes_canonical_lowercase_variants() {
        with_app_env(Some("production"), || {
            assert_eq!(Environment::detect(), Environment::Production);
        });
        with_app_env(Some("staging"), || {
            assert_eq!(Environment::detect(), Environment::Staging);
        });
        with_app_env(Some("development"), || {
            assert_eq!(Environment::detect(), Environment::Development);
        });
        with_app_env(Some("testing"), || {
            assert_eq!(Environment::detect(), Environment::Testing);
        });
        with_app_env(Some("local"), || {
            assert_eq!(Environment::detect(), Environment::Local);
        });
        with_app_env(None, || {
            assert_eq!(Environment::detect(), Environment::Local);
        });
    }

    #[test]
    #[serial_test::serial(app_config_env)]
    fn detect_is_case_insensitive_for_named_variants() {
        // The audit-cited case: APP_ENV=Production must NOT silently
        // become Custom("Production"), where is_production() returns
        // false and production-gated behavior gets skipped.
        for value in [
            "Production",
            "PRODUCTION",
            "ProDUCTion",
            "Staging",
            "STAGING",
            "Development",
            "DEVELOPMENT",
            "Testing",
            "TESTING",
            "Local",
            "LOCAL",
        ] {
            with_app_env(Some(value), || {
                let env = Environment::detect();
                assert!(
                    !matches!(env, Environment::Custom(_)),
                    "APP_ENV={value:?} fell through to Custom — case-insensitive match broken"
                );
            });
        }
    }

    #[test]
    #[serial_test::serial(app_config_env)]
    fn detect_accepts_common_aliases() {
        // The finding explicitly cites `prod`. Apply the same restraint
        // to the peer aliases — small documented set, no creative
        // expansion.
        with_app_env(Some("prod"), || {
            assert_eq!(Environment::detect(), Environment::Production);
            assert!(Environment::detect().is_production());
        });
        with_app_env(Some("PROD"), || {
            assert_eq!(Environment::detect(), Environment::Production);
        });
        with_app_env(Some("dev"), || {
            assert_eq!(Environment::detect(), Environment::Development);
        });
        with_app_env(Some("stage"), || {
            assert_eq!(Environment::detect(), Environment::Staging);
        });
        with_app_env(Some("stg"), || {
            assert_eq!(Environment::detect(), Environment::Staging);
        });
        with_app_env(Some("test"), || {
            assert_eq!(Environment::detect(), Environment::Testing);
        });
    }

    #[test]
    #[serial_test::serial(app_config_env)]
    fn detect_preserves_original_casing_for_custom_envs() {
        // Lowercasing the stored Custom value would silently change
        // which `.env.<suffix>` file loads — `APP_ENV=QA` must keep
        // loading `.env.QA`, not `.env.qa`.
        with_app_env(Some("QA"), || {
            let env = Environment::detect();
            assert_eq!(env, Environment::Custom("QA".to_string()));
            assert_eq!(env.env_file_suffix(), Some("QA"));
        });
        with_app_env(Some("Preview-Branch-42"), || {
            let env = Environment::detect();
            assert_eq!(env, Environment::Custom("Preview-Branch-42".to_string()));
            assert_eq!(env.env_file_suffix(), Some("Preview-Branch-42"));
        });
    }

    #[test]
    #[serial_test::serial(app_config_env)]
    fn is_production_recovers_for_capitalized_and_aliased_values() {
        // The audit's headline failure: Config-facing is_production()
        // must return true for the common casing/alias mistakes.
        for value in ["Production", "PRODUCTION", "prod", "PROD"] {
            with_app_env(Some(value), || {
                assert!(
                    Environment::detect().is_production(),
                    "APP_ENV={value:?} should still resolve to Production"
                );
            });
        }
    }
}
