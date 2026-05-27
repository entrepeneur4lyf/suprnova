//! Application path helpers — Suprnova's analogue of Laravel's
//! `base_path()` / `storage_path()` / `config_path()` family.
//!
//! Every path derives from a single base directory. The base defaults to the
//! process working directory and can be overridden with the `APP_BASE_PATH`
//! environment variable or [`set_base_path`]. Individual directories can be
//! redirected with the `use_*_path` setters for tests or non-standard
//! deployments.
//!
//! ```rust,ignore
//! use suprnova::storage_path;
//!
//! let down = storage_path("framework/down"); // <base>/storage/framework/down
//! let dir = storage_path("");                 // <base>/storage
//! ```

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

/// The resolved base directory plus any per-directory overrides.
#[derive(Debug, Clone)]
struct PathConfig {
    base: PathBuf,
    config: Option<PathBuf>,
    database: Option<PathBuf>,
    public: Option<PathBuf>,
    storage: Option<PathBuf>,
    resource: Option<PathBuf>,
    lang: Option<PathBuf>,
}

impl PathConfig {
    /// Resolve the base from `APP_BASE_PATH`, falling back to the current
    /// working directory (and `.` if even that is unavailable).
    fn detect() -> Self {
        let base = std::env::var_os("APP_BASE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        Self {
            base,
            config: None,
            database: None,
            public: None,
            storage: None,
            resource: None,
            lang: None,
        }
    }

    /// The directory for a standard child: the explicit override if set,
    /// otherwise `base/<default_child>`.
    fn dir(&self, override_dir: &Option<PathBuf>, default_child: &str) -> PathBuf {
        override_dir
            .clone()
            .unwrap_or_else(|| self.base.join(default_child))
    }
}

/// Join a relative path onto a directory, treating an empty relative path as
/// "the directory itself" (so `storage_path("")` returns the storage dir, not
/// a path with a trailing separator).
fn join(dir: PathBuf, rel: impl AsRef<Path>) -> PathBuf {
    let rel = rel.as_ref();
    if rel.as_os_str().is_empty() {
        dir
    } else {
        dir.join(rel)
    }
}

fn paths() -> &'static RwLock<PathConfig> {
    static PATHS: OnceLock<RwLock<PathConfig>> = OnceLock::new();
    PATHS.get_or_init(|| RwLock::new(PathConfig::detect()))
}

// Path resolution recovers in place on a poisoned lock (the hot-path registry
// policy): a panic elsewhere in the process must never make path lookups start
// failing. The config is tiny, so cloning the snapshot per call is cheap.
fn snapshot() -> PathConfig {
    paths().read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn with_mut(f: impl FnOnce(&mut PathConfig)) {
    let mut guard = paths().write().unwrap_or_else(|e| e.into_inner());
    f(&mut guard);
}

/// Path to the application base directory, optionally joined with `path`.
pub fn base_path(path: impl AsRef<Path>) -> PathBuf {
    join(snapshot().base, path)
}

/// Path to the `config` directory (`<base>/config`), optionally joined with `path`.
pub fn config_path(path: impl AsRef<Path>) -> PathBuf {
    let c = snapshot();
    join(c.dir(&c.config, "config"), path)
}

/// Path to the `database` directory (`<base>/database`), optionally joined with `path`.
pub fn database_path(path: impl AsRef<Path>) -> PathBuf {
    let c = snapshot();
    join(c.dir(&c.database, "database"), path)
}

/// Path to the `public` directory (`<base>/public`), optionally joined with `path`.
pub fn public_path(path: impl AsRef<Path>) -> PathBuf {
    let c = snapshot();
    join(c.dir(&c.public, "public"), path)
}

/// Path to the `storage` directory (`<base>/storage`), optionally joined with `path`.
pub fn storage_path(path: impl AsRef<Path>) -> PathBuf {
    let c = snapshot();
    join(c.dir(&c.storage, "storage"), path)
}

/// Path to the `resources` directory (`<base>/resources`), optionally joined with `path`.
pub fn resource_path(path: impl AsRef<Path>) -> PathBuf {
    let c = snapshot();
    join(c.dir(&c.resource, "resources"), path)
}

/// Path to the `lang` directory (`<base>/lang`), optionally joined with `path`.
///
/// This is where the translation module loads locale files from.
pub fn lang_path(path: impl AsRef<Path>) -> PathBuf {
    let c = snapshot();
    join(c.dir(&c.lang, "lang"), path)
}

/// Override the application base directory. All derived paths that don't have
/// their own override follow it.
pub fn set_base_path(path: impl Into<PathBuf>) {
    with_mut(|c| c.base = path.into());
}

/// Override the `config` directory independently of the base.
pub fn use_config_path(path: impl Into<PathBuf>) {
    with_mut(|c| c.config = Some(path.into()));
}

/// Override the `database` directory independently of the base.
pub fn use_database_path(path: impl Into<PathBuf>) {
    with_mut(|c| c.database = Some(path.into()));
}

/// Override the `public` directory independently of the base.
pub fn use_public_path(path: impl Into<PathBuf>) {
    with_mut(|c| c.public = Some(path.into()));
}

/// Override the `storage` directory independently of the base.
pub fn use_storage_path(path: impl Into<PathBuf>) {
    with_mut(|c| c.storage = Some(path.into()));
}

/// Override the `resources` directory independently of the base.
pub fn use_resource_path(path: impl Into<PathBuf>) {
    with_mut(|c| c.resource = Some(path.into()));
}

/// Override the `lang` directory independently of the base.
pub fn use_lang_path(path: impl Into<PathBuf>) {
    with_mut(|c| c.lang = Some(path.into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests exercise `PathConfig` directly rather than the global setters, so
    // they never mutate process-wide state and stay parallel-safe.
    fn cfg() -> PathConfig {
        PathConfig {
            base: PathBuf::from("/srv/app"),
            config: None,
            database: None,
            public: None,
            storage: None,
            resource: None,
            lang: None,
        }
    }

    #[test]
    fn derives_standard_dirs_from_base() {
        let c = cfg();
        assert_eq!(
            join(c.dir(&c.storage, "storage"), "framework/down"),
            PathBuf::from("/srv/app/storage/framework/down")
        );
        assert_eq!(
            join(c.dir(&c.database, "database"), "database.sqlite"),
            PathBuf::from("/srv/app/database/database.sqlite")
        );
        assert_eq!(
            join(c.dir(&c.lang, "lang"), "en.json"),
            PathBuf::from("/srv/app/lang/en.json")
        );
    }

    #[test]
    fn empty_relative_returns_the_directory_itself() {
        let c = cfg();
        assert_eq!(join(c.base.clone(), ""), PathBuf::from("/srv/app"));
        assert_eq!(
            join(c.dir(&c.config, "config"), ""),
            PathBuf::from("/srv/app/config")
        );
    }

    #[test]
    fn per_directory_override_redirects_only_that_dir() {
        let mut c = cfg();
        c.storage = Some(PathBuf::from("/var/lib/data"));
        assert_eq!(
            join(c.dir(&c.storage, "storage"), "cache"),
            PathBuf::from("/var/lib/data/cache")
        );
        // Other dirs still derive from the base.
        assert_eq!(
            join(c.dir(&c.public, "public"), ""),
            PathBuf::from("/srv/app/public")
        );
    }

    #[test]
    fn global_default_resolves_without_panicking() {
        // Smoke test of the process-wide accessor: it must resolve to an
        // absolute path (cwd or APP_BASE_PATH) and never panic.
        let base = base_path("");
        assert!(base.is_absolute() || base == Path::new("."));
        assert!(storage_path("framework/down").ends_with("storage/framework/down"));
    }
}
