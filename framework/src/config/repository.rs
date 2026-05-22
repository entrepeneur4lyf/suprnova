use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// Global config repository - stores config instances by type
static CONFIG_REPOSITORY: OnceLock<RwLock<ConfigRepository>> = OnceLock::new();

/// Repository for storing typed configuration structs
pub struct ConfigRepository {
    configs: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl ConfigRepository {
    /// Create a new empty config repository
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
        }
    }

    /// Register a config struct in the repository
    pub fn register<T: Any + Send + Sync + 'static>(&mut self, config: T) {
        self.configs.insert(TypeId::of::<T>(), Box::new(config));
    }

    /// Get a config struct by type
    pub fn get<T: Any + Send + Sync + Clone + 'static>(&self) -> Option<T> {
        self.configs
            .get(&TypeId::of::<T>())
            .and_then(|boxed| boxed.downcast_ref::<T>())
            .cloned()
    }

    /// Check if a config type is registered
    pub fn has<T: Any + 'static>(&self) -> bool {
        self.configs.contains_key(&TypeId::of::<T>())
    }
}

impl Default for ConfigRepository {
    fn default() -> Self {
        Self::new()
    }
}

/// Initialize the global config repository
pub fn init_repository() -> &'static RwLock<ConfigRepository> {
    CONFIG_REPOSITORY.get_or_init(|| RwLock::new(ConfigRepository::new()))
}

/// Register a config in the global repository.
///
/// A poisoned write lock — possible if another thread panicked while
/// holding the lock during boot — is recovered via
/// `PoisonError::into_inner` rather than silently dropping the
/// registration. Silent failure here would mean a custom `DatabaseConfig`
/// or `MailConfig` vanishes from the repository for the rest of the
/// process lifetime; every `Config::get::<T>()` after that returns None,
/// and the framework falls back to defaults invisibly.
pub fn register<T: Any + Send + Sync + 'static>(config: T) {
    let repo = init_repository();
    let mut guard = match repo.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.register(config);
}

/// Get a config from the global repository.
///
/// Poisoned read locks recover via `PoisonError::into_inner` so a panic
/// during a prior `register` cannot silently make every subsequent
/// `Config::get::<T>()` return None.
pub fn get<T: Any + Send + Sync + Clone + 'static>() -> Option<T> {
    let repo = CONFIG_REPOSITORY.get()?;
    let guard = match repo.read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.get::<T>()
}

/// Check if a config type is registered in the global repository.
///
/// Poisoned read locks recover via `PoisonError::into_inner` for the
/// same reason as [`get`].
pub fn has<T: Any + 'static>() -> bool {
    let Some(repo) = CONFIG_REPOSITORY.get() else {
        return false;
    };
    let guard = match repo.read() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.has::<T>()
}
