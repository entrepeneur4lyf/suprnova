//! Application Container for Dependency Injection
//!
//! This module provides Laravel-like service container capabilities:
//! - Singletons: one stored value per type, returned on every resolution
//! - Factories: new instance per resolution
//! - Trait bindings: bind interfaces to implementations behind `Arc<dyn Trait>`
//! - Test faking: swap implementations in tests
//! - Service Providers: bootstrap services with register/boot lifecycle
//!
//! # Sharing semantics
//!
//! Laravel's `singleton` returns the same object reference every time. Rust
//! has no implicit shared-by-reference for owned values, so Suprnova exposes
//! two flavours that map to the same intent:
//!
//! - **Trait bindings — `App::bind::<dyn Trait>(Arc::new(impl))`**: the
//!   stored `Arc<dyn Trait>` is cloned on resolution. All callers see the
//!   same underlying object. This is the recommended shape whenever shared
//!   state matters (caches, brokers, drivers, hubs, registries).
//!
//! - **Concrete `App::singleton::<T>(value)`**: the value is stored once
//!   and `App::get::<T>()` returns a `Clone` of it on every resolution.
//!   `T` must therefore implement [`Clone`]. For `Copy` / cheap-clone
//!   types this is fine, and for `Arc<...>` / `Arc<Mutex<...>>` the
//!   clone is a refcount bump so callers still see the same inner state.
//!   For a plain non-trivial struct with mutable interior state, wrap it
//!   in `Arc<Mutex<T>>` (or bind a trait) before registering — bare
//!   `singleton::<T>(...)` would otherwise give each resolver an
//!   independent copy of the data.
//!
//! # Lock-poisoning policy
//!
//! The global container guards itself behind a `std::sync::RwLock`. Every
//! write path on [`App`] (`singleton`, `factory`, `bind`, `bind_factory`,
//! and the test-container `instance` alias) recovers from a poisoned lock
//! via `unwrap_or_else(|e| e.into_inner())` so service registration cannot
//! be silently dropped if some unrelated subsystem panicked mid-write.
//! Reads recover the same way, so a poisoned container still resolves
//! whatever was registered before the poison. This matches the
//! recover-in-place pattern used elsewhere in the framework for
//! hot-path registries (see the `lock` module's `read`/`write`/`lock`
//! helpers — `pub(crate)`, not part of the consumer surface).
//!
//! # Example
//!
//! ```rust,no_run
//! # use std::sync::Arc;
//! # use suprnova::{App, bind, singleton};
//! // Define a service trait for the container.
//! pub trait HttpClient: Send + Sync {
//!     fn get(&self, url: &str) -> String;
//! }
//!
//! # struct RealHttpClient;
//! # impl RealHttpClient { fn new() -> Self { RealHttpClient } }
//! # impl HttpClient for RealHttpClient { fn get(&self, _url: &str) -> String { String::new() } }
//! # #[derive(Clone)]
//! # struct CacheService;
//! # impl CacheService { fn new() -> Self { CacheService } }
//! # fn ex() {
//! // Register implementations using the macros.
//! bind!(dyn HttpClient, RealHttpClient::new());
//! singleton!(CacheService::new());
//!
//! // Resolve anywhere in your app.
//! let client: Arc<dyn HttpClient> = App::make::<dyn HttpClient>().unwrap();
//! # let _ = client;
//! # }
//! ```

pub mod provider;
pub mod testing;

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

/// Global application container
static APP_CONTAINER: OnceLock<RwLock<Container>> = OnceLock::new();

// Thread-local test overrides for isolated testing.
//
// Suitable for sync tests and `#[tokio::test]` running on the default
// `current_thread` flavour. NOT safe across `flavor = "multi_thread"`
// or `tokio::spawn` boundaries — see [`TASK_CONTAINER`] for the
// async-safe alternative.
thread_local! {
    pub(crate) static TEST_CONTAINER: RefCell<Option<Container>> = const { RefCell::new(None) };
}

// Task-local test overrides for async-safe isolated testing.
//
// `tokio::task_local!` is per-async-task, so the override persists for
// the entire future regardless of which worker thread the runtime
// picks up the future on. This closes the audit-flagged hole where
// `TEST_CONTAINER` (thread-local) could become invisible on a
// multi-thread runtime when the future migrated to a different worker.
//
// Lookups in `App` consult this first, then [`TEST_CONTAINER`], then
// the global container — so existing `TestContainer::fake()` callers
// keep working unchanged while new tests opt into the async-safe path
// via [`testing::TestContainer::scope`].
//
// Note on `tokio::spawn`: bare `tokio::spawn`'d child tasks do NOT
// inherit task-locals. Tests that spawn sub-tasks needing access to
// the override should use [`testing::TestContainer::spawn`] instead
// — it captures the current task-local container and re-installs it
// inside the spawned future, so the fakes remain visible across the
// spawn boundary.
tokio::task_local! {
    pub(crate) static TASK_CONTAINER: Arc<RwLock<Container>>;
}

/// Binding types: either a singleton instance or a factory closure
#[derive(Clone)]
enum Binding {
    /// Shared singleton instance - same instance returned every time
    Singleton(Arc<dyn Any + Send + Sync>),

    /// Factory closure - creates new instance each time
    Factory(Arc<dyn Fn() -> Arc<dyn Any + Send + Sync> + Send + Sync>),
}

impl Binding {
    /// Resolve this binding to a concrete `T: Clone` value. Lock-free: the
    /// caller is expected to have already cloned the binding out of the
    /// container's `RwLock` (see [`Container::binding`]) so factory
    /// closures do not run while a read or write guard is held.
    fn resolve_concrete<T: Any + Send + Sync + Clone + 'static>(&self) -> Option<T> {
        match self {
            Binding::Singleton(arc) => arc.downcast_ref::<T>().cloned(),
            Binding::Factory(factory) => {
                let arc = factory();
                arc.downcast_ref::<T>().cloned()
            }
        }
    }

    /// Resolve this binding to an `Arc<T>` trait object. Lock-free; see
    /// [`Binding::resolve_concrete`] for the lock-handling contract.
    fn resolve_make<T: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        match self {
            Binding::Singleton(arc) => arc.downcast_ref::<Arc<T>>().cloned(),
            Binding::Factory(factory) => {
                let arc = factory();
                arc.downcast_ref::<Arc<T>>().cloned()
            }
        }
    }
}

/// The main service container
///
/// Stores type-erased bindings keyed by TypeId. Supports both concrete types
/// and trait objects (via `Arc<dyn Trait>`).
pub struct Container {
    /// Type bindings: TypeId -> Binding
    bindings: HashMap<TypeId, Binding>,
    /// Inertia shared-data registry. One per Container instance — scoped
    /// to either the global app or a `TestContainer::fake()` test override.
    inertia: Arc<crate::inertia::InertiaRegistry>,
}

impl Container {
    /// Create a new empty container
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
            inertia: Arc::new(crate::inertia::InertiaRegistry::new()),
        }
    }

    /// Get the Inertia shared-data registry for this container.
    pub fn inertia(&self) -> &Arc<crate::inertia::InertiaRegistry> {
        &self.inertia
    }

    /// Register a singleton instance (shared across all resolutions)
    ///
    /// # Example
    /// ```rust,no_run
    /// # use suprnova::Container;
    /// # #[derive(Clone)]
    /// # struct DatabaseConnection;
    /// # impl DatabaseConnection { fn new(_url: &str) -> Self { DatabaseConnection } }
    /// # let url = "postgres://localhost/app";
    /// # let mut container = Container::new();
    /// container.singleton(DatabaseConnection::new(&url));
    /// ```
    pub fn singleton<T: Any + Send + Sync + 'static>(&mut self, instance: T) {
        let arc: Arc<dyn Any + Send + Sync> = Arc::new(instance);
        self.bindings
            .insert(TypeId::of::<T>(), Binding::Singleton(arc));
    }

    /// Register a factory closure (new instance per resolution)
    ///
    /// # Example
    /// ```rust,no_run
    /// # use suprnova::Container;
    /// # struct RequestLogger;
    /// # impl RequestLogger { fn new() -> Self { RequestLogger } }
    /// # let mut container = Container::new();
    /// container.factory(|| RequestLogger::new());
    /// ```
    pub fn factory<T, F>(&mut self, factory: F)
    where
        T: Any + Send + Sync + 'static,
        F: Fn() -> T + Send + Sync + 'static,
    {
        let wrapped: Arc<dyn Fn() -> Arc<dyn Any + Send + Sync> + Send + Sync> =
            Arc::new(move || Arc::new(factory()) as Arc<dyn Any + Send + Sync>);
        self.bindings
            .insert(TypeId::of::<T>(), Binding::Factory(wrapped));
    }

    /// Bind a trait object to a concrete implementation (as singleton)
    ///
    /// This stores the value under `TypeId::of::<Arc<dyn Trait>>()` which allows
    /// trait objects to be resolved via `make::<dyn Trait>()`.
    ///
    /// Last write wins — calling `bind` again for the same trait overwrites the
    /// previous binding. Use [`Container::bind_if_absent`] when registering
    /// from boot hooks that may run more than once.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use suprnova::Container;
    /// # trait HttpClient: Send + Sync {}
    /// # struct RealHttpClient;
    /// # impl RealHttpClient { fn new() -> Self { RealHttpClient } }
    /// # impl HttpClient for RealHttpClient {}
    /// # let mut container = Container::new();
    /// container.bind::<dyn HttpClient>(Arc::new(RealHttpClient::new()));
    /// ```
    pub fn bind<T: ?Sized + Send + Sync + 'static>(&mut self, instance: Arc<T>) {
        // Store under TypeId of Arc<T> (works for both concrete and trait objects)
        let type_id = TypeId::of::<Arc<T>>();
        let arc: Arc<dyn Any + Send + Sync> = Arc::new(instance);
        self.bindings.insert(type_id, Binding::Singleton(arc));
    }

    /// Bind a trait object to a concrete implementation only if no binding
    /// already exists for that trait. Returns `true` if the binding was
    /// installed, `false` if a binding was already present.
    ///
    /// This is the idempotent variant used by `#[service]` auto-registration so
    /// that re-running boot does not clobber manual overrides or stateful
    /// singletons.
    pub fn bind_if_absent<T: ?Sized + Send + Sync + 'static>(&mut self, instance: Arc<T>) -> bool {
        let type_id = TypeId::of::<Arc<T>>();
        if self.bindings.contains_key(&type_id) {
            return false;
        }
        let arc: Arc<dyn Any + Send + Sync> = Arc::new(instance);
        self.bindings.insert(type_id, Binding::Singleton(arc));
        true
    }

    /// Register a singleton instance only if none is registered for the type.
    /// Returns `true` if the singleton was installed, `false` if a binding for
    /// that type already exists.
    ///
    /// This is the idempotent variant used by `#[injectable]` auto-registration
    /// so that re-running boot does not replace runtime state with a fresh
    /// `Default::default()` value.
    pub fn singleton_if_absent<T: Any + Send + Sync + 'static>(&mut self, instance: T) -> bool {
        let type_id = TypeId::of::<T>();
        if self.bindings.contains_key(&type_id) {
            return false;
        }
        let arc: Arc<dyn Any + Send + Sync> = Arc::new(instance);
        self.bindings.insert(type_id, Binding::Singleton(arc));
        true
    }

    /// Bind a trait object to a factory
    ///
    /// # Example
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use suprnova::Container;
    /// # trait HttpClient: Send + Sync {}
    /// # struct RealHttpClient;
    /// # impl RealHttpClient { fn new() -> Self { RealHttpClient } }
    /// # impl HttpClient for RealHttpClient {}
    /// # let mut container = Container::new();
    /// container.bind_factory::<dyn HttpClient, _>(|| Arc::new(RealHttpClient::new()));
    /// ```
    pub fn bind_factory<T: ?Sized + Send + Sync + 'static, F>(&mut self, factory: F)
    where
        F: Fn() -> Arc<T> + Send + Sync + 'static,
    {
        let type_id = TypeId::of::<Arc<T>>();
        let wrapped: Arc<dyn Fn() -> Arc<dyn Any + Send + Sync> + Send + Sync> =
            Arc::new(move || Arc::new(factory()) as Arc<dyn Any + Send + Sync>);
        self.bindings.insert(type_id, Binding::Factory(wrapped));
    }

    /// Clone the binding for `type_id` out of the map. Returned `Binding`
    /// is cheap to clone (both variants are `Arc`s) and can be resolved
    /// after any surrounding lock has been released — used by `App::get`
    /// and `App::make` to avoid running factory closures while a read
    /// guard on the container is still alive.
    fn binding(&self, type_id: TypeId) -> Option<Binding> {
        self.bindings.get(&type_id).cloned()
    }

    /// Resolve a concrete type (requires Clone)
    ///
    /// # Example
    /// ```rust,no_run
    /// # use suprnova::Container;
    /// # #[derive(Clone)]
    /// # struct DatabaseConnection;
    /// # let container = Container::new();
    /// let db: DatabaseConnection = container.get().unwrap();
    /// # let _ = db;
    /// ```
    pub fn get<T: Any + Send + Sync + Clone + 'static>(&self) -> Option<T> {
        self.binding(TypeId::of::<T>())?.resolve_concrete::<T>()
    }

    /// Resolve a trait binding — returns `Arc<T>`.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use suprnova::Container;
    /// # trait HttpClient: Send + Sync {}
    /// # let container = Container::new();
    /// let client: Arc<dyn HttpClient> = container.make::<dyn HttpClient>().unwrap();
    /// # let _ = client;
    /// ```
    pub fn make<T: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.binding(TypeId::of::<Arc<T>>())?.resolve_make::<T>()
    }

    /// Check if a concrete type is registered
    pub fn has<T: Any + 'static>(&self) -> bool {
        self.bindings.contains_key(&TypeId::of::<T>())
    }

    /// Check if a trait binding is registered
    pub fn has_binding<T: ?Sized + 'static>(&self) -> bool {
        self.bindings.contains_key(&TypeId::of::<Arc<T>>())
    }
}

impl Default for Container {
    fn default() -> Self {
        Self::new()
    }
}

/// Application container facade
///
/// Provides static methods for service registration and resolution.
/// Uses a global container with thread-local test overrides.
///
/// # Example
///
/// ```rust,no_run
/// # use std::sync::Arc;
/// use suprnova::{App, bind, singleton};
/// # #[derive(Clone)]
/// # struct DatabaseConnection;
/// # impl DatabaseConnection { fn new(_url: &str) -> Self { DatabaseConnection } }
/// # trait HttpClient: Send + Sync {}
/// # struct RealHttpClient;
/// # impl RealHttpClient { fn new() -> Self { RealHttpClient } }
/// # impl HttpClient for RealHttpClient {}
/// # fn ex(url: &str) {
/// // Register services at startup using macros
/// singleton!(DatabaseConnection::new(&url));
/// bind!(dyn HttpClient, RealHttpClient::new());
///
/// // Resolve anywhere
/// let db: DatabaseConnection = App::get().unwrap();
/// let client: Arc<dyn HttpClient> = App::make::<dyn HttpClient>().unwrap();
/// # let _ = (db, client);
/// # }
/// ```
pub struct App;

impl App {
    /// Initialize the application container
    ///
    /// Should be called once at application startup. This is automatically
    /// called by `Server::from_config()`.
    pub fn init() {
        APP_CONTAINER.get_or_init(|| RwLock::new(Container::new()));
    }

    /// Register a singleton instance.
    ///
    /// One value is stored per `TypeId::<T>`; [`App::get::<T>`] returns a
    /// [`Clone`] of it on every resolution. For shared mutable state wrap
    /// `T` in `Arc<Mutex<...>>` (or use [`App::bind`] with a trait object)
    /// — see the module docs for the full sharing-semantics note.
    ///
    /// Recovers in place from a poisoned container lock so the registration
    /// is never silently dropped.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use suprnova::App;
    /// # #[derive(Clone)]
    /// # struct DatabaseConnection;
    /// # impl DatabaseConnection { fn new(_url: &str) -> Self { DatabaseConnection } }
    /// # let url = "postgres://localhost/app";
    /// App::singleton(DatabaseConnection::new(&url));
    /// ```
    pub fn singleton<T: Any + Send + Sync + 'static>(instance: T) {
        let container = APP_CONTAINER.get_or_init(|| RwLock::new(Container::new()));
        let mut c = container.write().unwrap_or_else(|e| e.into_inner());
        c.singleton(instance);
    }

    /// Register an existing instance as a shared singleton.
    ///
    /// Laravel-named alias of [`App::singleton`] — mirrors
    /// `$container->instance($abstract, $instance)`. In Laravel this is the
    /// "I already constructed this thing, just remember it" call; Suprnova's
    /// `singleton` accepts a value directly so the two share the same
    /// implementation. Provided so code migrating from Laravel keeps reading
    /// fluently.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use suprnova::App;
    /// # #[derive(Clone)]
    /// # struct DatabaseConnection;
    /// # impl DatabaseConnection { fn new(_url: &str) -> Self { DatabaseConnection } }
    /// # let url = "postgres://localhost/app";
    /// App::instance(DatabaseConnection::new(&url));
    /// ```
    pub fn instance<T: Any + Send + Sync + 'static>(value: T) {
        Self::singleton(value);
    }

    /// Register a factory binding (new instance per resolution).
    ///
    /// Recovers in place from a poisoned container lock so the registration
    /// is never silently dropped.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use suprnova::App;
    /// # struct RequestLogger;
    /// # impl RequestLogger { fn new() -> Self { RequestLogger } }
    /// App::factory(|| RequestLogger::new());
    /// ```
    pub fn factory<T, F>(factory: F)
    where
        T: Any + Send + Sync + 'static,
        F: Fn() -> T + Send + Sync + 'static,
    {
        let container = APP_CONTAINER.get_or_init(|| RwLock::new(Container::new()));
        let mut c = container.write().unwrap_or_else(|e| e.into_inner());
        c.factory(factory);
    }

    /// Bind a trait object to a concrete implementation (as singleton).
    ///
    /// Last write wins — calling `bind` again for the same trait overwrites
    /// the previous binding. Use [`App::bind_if_absent`] when registering from
    /// boot hooks that may run more than once.
    ///
    /// Recovers in place from a poisoned container lock so the binding is
    /// never silently dropped.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use suprnova::App;
    /// # trait HttpClient: Send + Sync {}
    /// # struct RealHttpClient;
    /// # impl RealHttpClient { fn new() -> Self { RealHttpClient } }
    /// # impl HttpClient for RealHttpClient {}
    /// App::bind::<dyn HttpClient>(Arc::new(RealHttpClient::new()));
    /// ```
    pub fn bind<T: ?Sized + Send + Sync + 'static>(instance: Arc<T>) {
        let container = APP_CONTAINER.get_or_init(|| RwLock::new(Container::new()));
        let mut c = container.write().unwrap_or_else(|e| e.into_inner());
        c.bind(instance);
    }

    /// Bind a trait object only if no binding already exists for that trait.
    /// Returns `true` if the binding was installed, `false` if a binding was
    /// already present. Used by `#[service]` auto-registration so re-running
    /// boot does not clobber manual overrides or stateful singletons.
    ///
    /// Manual `App::bind` calls always override, so application code retains
    /// the ability to replace a default-registered service explicitly.
    ///
    /// Recovers in place from a poisoned container lock — the binding is
    /// honoured against whatever was registered before the poison.
    pub fn bind_if_absent<T: ?Sized + Send + Sync + 'static>(instance: Arc<T>) -> bool {
        let container = APP_CONTAINER.get_or_init(|| RwLock::new(Container::new()));
        let mut c = container.write().unwrap_or_else(|e| e.into_inner());
        c.bind_if_absent(instance)
    }

    /// Register a singleton only if none is registered for the concrete type.
    /// Returns `true` if the singleton was installed, `false` if a binding for
    /// that type already exists. Used by `#[injectable]` auto-registration so
    /// re-running boot does not replace runtime state with a fresh
    /// `Default::default()` value.
    ///
    /// Manual `App::singleton` calls always override, so application code can
    /// still install a custom instance after boot.
    ///
    /// Recovers in place from a poisoned container lock — the registration
    /// is honoured against whatever was registered before the poison.
    pub fn singleton_if_absent<T: Any + Send + Sync + 'static>(instance: T) -> bool {
        let container = APP_CONTAINER.get_or_init(|| RwLock::new(Container::new()));
        let mut c = container.write().unwrap_or_else(|e| e.into_inner());
        c.singleton_if_absent(instance)
    }

    /// Bind a trait object to a factory.
    ///
    /// Recovers in place from a poisoned container lock so the binding is
    /// never silently dropped.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use suprnova::App;
    /// # trait HttpClient: Send + Sync {}
    /// # struct RealHttpClient;
    /// # impl RealHttpClient { fn new() -> Self { RealHttpClient } }
    /// # impl HttpClient for RealHttpClient {}
    /// App::bind_factory::<dyn HttpClient, _>(|| {
    ///     Arc::new(RealHttpClient::new()) as Arc<dyn HttpClient>
    /// });
    /// ```
    pub fn bind_factory<T: ?Sized + Send + Sync + 'static, F>(factory: F)
    where
        F: Fn() -> Arc<T> + Send + Sync + 'static,
    {
        let container = APP_CONTAINER.get_or_init(|| RwLock::new(Container::new()));
        let mut c = container.write().unwrap_or_else(|e| e.into_inner());
        c.bind_factory(factory);
    }

    /// Resolve a concrete type.
    ///
    /// Lookup order:
    /// 1. Task-local test override ([`testing::TestContainer::scope`]) —
    ///    async-safe across multi-thread runtimes.
    /// 2. Thread-local test override ([`testing::TestContainer::fake`]) —
    ///    sync / `current_thread` tests.
    /// 3. Global container — production lookup.
    ///
    /// All three layers recover in place from a poisoned lock so a panic
    /// in one registration does not turn every later resolution into a
    /// silent service-not-found.
    ///
    /// Factory closures run AFTER the container lock is released. The
    /// binding is cloned out from under the read guard and only then
    /// invoked, so a factory may safely call back into `App::*` or run
    /// arbitrarily expensive work without blocking concurrent writers
    /// or deadlocking against a re-entrant write.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use suprnova::App;
    /// # #[derive(Clone)]
    /// # struct DatabaseConnection;
    /// let db: DatabaseConnection = App::get().unwrap();
    /// # let _ = db;
    /// ```
    pub fn get<T: Any + Send + Sync + Clone + 'static>() -> Option<T> {
        let type_id = TypeId::of::<T>();

        // Task-local first (async-safe). Clone the binding out from under
        // the read guard so any factory closure runs lock-free — otherwise
        // a factory that re-enters `App::*` (or any writer) would deadlock,
        // and an expensive factory would needlessly block container mutation.
        if let Some(binding) = TASK_CONTAINER
            .try_with(|c| c.read().unwrap_or_else(|e| e.into_inner()).binding(type_id))
            .ok()
            .flatten()
        {
            return binding.resolve_concrete::<T>();
        }

        // Thread-local second (sync / current_thread compat). RefCell so
        // there's no cross-thread guard, but we extract the binding before
        // invoking it to keep the resolution shape uniform across layers.
        let test_binding = TEST_CONTAINER.with(|c| {
            c.borrow()
                .as_ref()
                .and_then(|container| container.binding(type_id))
        });
        if let Some(binding) = test_binding {
            return binding.resolve_concrete::<T>();
        }

        // Fall back to global container. Same extract-then-drop-lock shape.
        let container = APP_CONTAINER.get()?;
        let binding = container
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .binding(type_id)?;
        binding.resolve_concrete::<T>()
    }

    /// Resolve a trait binding — returns `Arc<T>`.
    ///
    /// Lookup order:
    /// 1. Task-local test override ([`testing::TestContainer::scope`]) —
    ///    async-safe across multi-thread runtimes.
    /// 2. Thread-local test override ([`testing::TestContainer::fake`]) —
    ///    sync / `current_thread` tests.
    /// 3. Global container — production lookup.
    ///
    /// All three layers recover in place from a poisoned lock so a panic
    /// in one registration does not turn every later resolution into a
    /// silent service-not-found.
    ///
    /// Factory closures run AFTER the container lock is released — see
    /// [`App::get`] for the full contract; the same guarantee holds for
    /// trait factories registered via [`App::bind_factory`].
    ///
    /// # Example
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use suprnova::App;
    /// # trait HttpClient: Send + Sync {}
    /// let client: Arc<dyn HttpClient> = App::make::<dyn HttpClient>().unwrap();
    /// # let _ = client;
    /// ```
    pub fn make<T: ?Sized + Send + Sync + 'static>() -> Option<Arc<T>> {
        let type_id = TypeId::of::<Arc<T>>();

        // Task-local first (async-safe). Same extract-then-drop-lock shape
        // as `App::get` so factory closures that re-enter `App::*` cannot
        // deadlock against a held read guard.
        if let Some(binding) = TASK_CONTAINER
            .try_with(|c| c.read().unwrap_or_else(|e| e.into_inner()).binding(type_id))
            .ok()
            .flatten()
        {
            return binding.resolve_make::<T>();
        }

        // Thread-local second (sync / current_thread compat).
        let test_binding = TEST_CONTAINER.with(|c| {
            c.borrow()
                .as_ref()
                .and_then(|container| container.binding(type_id))
        });
        if let Some(binding) = test_binding {
            return binding.resolve_make::<T>();
        }

        // Fall back to global container.
        let container = APP_CONTAINER.get()?;
        let binding = container
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .binding(type_id)?;
        binding.resolve_make::<T>()
    }

    /// Resolve a concrete type, returning an error if not found
    ///
    /// This allows using the `?` operator in controllers and services for
    /// automatic error propagation with proper HTTP responses.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use suprnova::App;
    /// # #[derive(Clone)]
    /// # struct MyService;
    /// # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let service = App::resolve::<MyService>()?;
    /// // ... use `service` to handle the request ...
    /// # let _ = service;
    /// # Ok(()) }
    /// ```
    pub fn resolve<T: Any + Send + Sync + Clone + 'static>()
    -> Result<T, crate::error::FrameworkError> {
        Self::get::<T>().ok_or_else(crate::error::FrameworkError::service_not_found::<T>)
    }

    /// Resolve a trait binding, returning an error if not found
    ///
    /// This allows using the `?` operator for trait object resolution.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use std::sync::Arc;
    /// # use suprnova::App;
    /// # trait HttpClient: Send + Sync {}
    /// # fn ex() -> Result<(), Box<dyn std::error::Error>> {
    /// let client: Arc<dyn HttpClient> = App::resolve_make::<dyn HttpClient>()?;
    /// # let _ = client;
    /// # Ok(()) }
    /// ```
    pub fn resolve_make<T: ?Sized + Send + Sync + 'static>()
    -> Result<Arc<T>, crate::error::FrameworkError> {
        Self::make::<T>().ok_or_else(crate::error::FrameworkError::service_not_found::<T>)
    }

    /// Check if a concrete type is registered.
    ///
    /// Lookup order matches [`App::get`]: task-local, thread-local, global.
    /// Recovers in place from a poisoned lock at every layer.
    pub fn has<T: Any + 'static>() -> bool {
        // Task-local first.
        if TASK_CONTAINER
            .try_with(|c| c.read().unwrap_or_else(|e| e.into_inner()).has::<T>())
            .unwrap_or(false)
        {
            return true;
        }

        // Thread-local second.
        let in_test = TEST_CONTAINER.with(|c| {
            c.borrow()
                .as_ref()
                .map(|container| container.has::<T>())
                .unwrap_or(false)
        });

        if in_test {
            return true;
        }

        APP_CONTAINER
            .get()
            .map(|c| c.read().unwrap_or_else(|e| e.into_inner()).has::<T>())
            .unwrap_or(false)
    }

    /// Check if a trait binding is registered.
    ///
    /// Lookup order matches [`App::make`]: task-local, thread-local, global.
    /// Recovers in place from a poisoned lock at every layer.
    pub fn has_binding<T: ?Sized + 'static>() -> bool {
        // Task-local first.
        if TASK_CONTAINER
            .try_with(|c| {
                c.read()
                    .unwrap_or_else(|e| e.into_inner())
                    .has_binding::<T>()
            })
            .unwrap_or(false)
        {
            return true;
        }

        // Thread-local second.
        let in_test = TEST_CONTAINER.with(|c| {
            c.borrow()
                .as_ref()
                .map(|container| container.has_binding::<T>())
                .unwrap_or(false)
        });

        if in_test {
            return true;
        }

        APP_CONTAINER
            .get()
            .map(|c| {
                c.read()
                    .unwrap_or_else(|e| e.into_inner())
                    .has_binding::<T>()
            })
            .unwrap_or(false)
    }

    /// Laravel-named alias for [`App::has`] — `bound::<T>()` returns true
    /// when a concrete type is registered with the container.
    ///
    /// Laravel's `$container->bound($abstract)` answers a single question
    /// over both type-bindings and trait-bindings. Suprnova keeps the two
    /// pools type-distinct ([`App::has`] for concrete `T`, [`App::bound`]
    /// for trait objects via [`App::bound_binding`]), so each call lands
    /// on the correct map without runtime string lookup.
    pub fn bound<T: Any + 'static>() -> bool {
        Self::has::<T>()
    }

    /// Laravel-named alias for [`App::has_binding`] — `bound_binding::<dyn
    /// Trait>()` returns true when a trait binding is registered.
    pub fn bound_binding<T: ?Sized + 'static>() -> bool {
        Self::has_binding::<T>()
    }

    /// Boot all auto-registered services.
    ///
    /// Registers everything declared via `#[service(ConcreteType)]` and
    /// `#[injectable]`. Service bindings run as a single pass; singletons run
    /// in a fixed-point loop so an `#[injectable]` type whose `#[inject]`
    /// fields name another `#[injectable]` resolves regardless of inventory
    /// iteration order. Returns a structured error if a singleton's
    /// dependencies cannot be resolved (missing `#[injectable]` or a cyclic
    /// dependency) rather than panicking inside the registration closure.
    ///
    /// Called automatically by `Server::from_config()`.
    pub fn boot_services() -> Result<(), crate::error::FrameworkError> {
        provider::bootstrap()
    }

    /// Resolve the active Inertia registry — test override if set, else
    /// the global container's. Used by both `App::inertia_share*` writes
    /// and `InertiaResponse::resolve` reads so tests that swap a
    /// `TestContainer::fake()` (thread-local) or `TestContainer::scope`
    /// (task-local) get clean isolation.
    ///
    /// Lookup order matches [`App::get`]: task-local, thread-local, global.
    /// Recovers in place from a poisoned lock at every layer rather than
    /// panicking — matches the rest of `App::*` reads/writes.
    pub fn inertia_registry() -> Arc<crate::inertia::InertiaRegistry> {
        // Task-local first (async-safe).
        if let Ok(reg) =
            TASK_CONTAINER.try_with(|c| c.read().unwrap_or_else(|e| e.into_inner()).inertia.clone())
        {
            return reg;
        }

        // Thread-local second (sync / current_thread compat).
        let test = TEST_CONTAINER.with(|c| {
            c.borrow()
                .as_ref()
                .map(|container| container.inertia.clone())
        });
        if let Some(reg) = test {
            return reg;
        }

        // Fall back to the global container, lazy-initializing if necessary
        // so callers don't have to remember to call `App::init` first.
        let container = APP_CONTAINER.get_or_init(|| RwLock::new(Container::new()));
        container
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .inertia
            .clone()
    }

    /// Register a synchronous Inertia shared prop. Included in every
    /// Inertia response (unless filtered by partial reload). Last write
    /// wins for a given key — call once per key at bootstrap time.
    ///
    /// Writes to the active container's registry: production writes to
    /// the global container; tests using `TestContainer::fake()` write
    /// to the per-test override.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use suprnova::App;
    /// # fn ex() {
    /// App::inertia_share("appName", "Suprnova");
    /// App::inertia_share("appVersion", env!("CARGO_PKG_VERSION"));
    /// # }
    /// ```
    pub fn inertia_share<V: serde::Serialize>(key: impl Into<String>, value: V) {
        Self::inertia_registry().share_value(key, value);
    }

    /// Register an async lazy Inertia shared prop. The resolver runs on
    /// every Inertia response where the prop is needed (i.e. not excluded
    /// by partial-reload filtering). Use when the shared value requires
    /// async work — DB lookups, HTTP calls, etc.
    ///
    /// # Example
    /// ```rust,no_run
    /// # use suprnova::App;
    /// # use suprnova::FrameworkError;
    /// # async fn detect_locale() -> String { "en".to_string() }
    /// # fn ex() {
    /// App::inertia_share_lazy("locale", || async {
    ///     Ok::<_, FrameworkError>(detect_locale().await)
    /// });
    /// # }
    /// ```
    pub fn inertia_share_lazy<F, Fut, V>(key: impl Into<String>, resolver: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<V, crate::error::FrameworkError>> + Send + 'static,
        V: serde::Serialize + 'static,
    {
        Self::inertia_registry().share_lazy(key, resolver);
    }

    /// Register the singleton [`crate::inertia::InertiaSharedData`]
    /// implementation. The framework calls `share(&req)` on every Inertia
    /// response, letting you produce per-request shared data
    /// (authenticated user, locale, flash messages, ...).
    pub fn register_inertia_shared(provider: Arc<dyn crate::inertia::InertiaSharedData>) {
        Self::inertia_registry().register_trait(provider);
    }

    /// Register an Inertia shared *once* prop — resolved on the first
    /// page that needs it, then cached on the client across navigations.
    /// Maps to `Inertia::shareOnce($k, fn() => ...)`.
    ///
    /// Use for shared data that's expensive to compute but rarely
    /// changes — locale lists, plan catalogs, navigation menus, etc.
    /// The client tracks the cache key and the framework skips the
    /// resolver via `X-Inertia-Except-Once-Props` on subsequent visits.
    pub fn inertia_share_once<F, Fut, V>(key: impl Into<String>, resolver: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<V, crate::error::FrameworkError>> + Send + 'static,
        V: serde::Serialize + 'static,
    {
        Self::inertia_registry().share_once(key, resolver);
    }

    /// Push a value into the current request's flash bag. Drained by
    /// the next Inertia response and emitted under `page.flash`. The
    /// flash bag is scoped per request via `tokio::task_local!` and is
    /// silently a no-op if called outside an HTTP request (e.g. from
    /// a background worker).
    ///
    /// **Cross-redirect persistence**: when the handler returns a
    /// [`Redirect`](crate::http::Redirect) and a session scope is
    /// active, the flash bag is transferred into the session on
    /// conversion to [`Response`](crate::http::Response) and surfaces
    /// on the receiving request's Inertia response under `page.flash`.
    /// Without a session scope the values still appear on the *current*
    /// response but cannot survive the redirect. Same-request flashes
    /// win over inherited session flashes on key collision.
    pub fn flash<V: serde::Serialize>(key: impl Into<String>, value: V) {
        let key = key.into();
        // Soft-fail on serialise error to match the sibling
        // `Redirect::with` path. A `HashMap<(i32, i32), &str>` or any
        // type whose `Serialize` impl returns Err would otherwise
        // abort the request via panic; instead we log + drop the
        // entry so the rest of the response renders normally.
        match serde_json::to_value(&value) {
            Ok(v) => crate::inertia::flash::push(key, v),
            Err(err) => {
                tracing::warn!(
                    flash_key = %key,
                    error = %err,
                    "App::flash value failed to serialise; dropping the entry. \
                     This typically means a HashMap with non-string keys, or a \
                     custom Serialize impl returned Err — both are caller bugs."
                );
            }
        }
    }

    /// Disable Inertia SSR for the remainder of this request. Equivalent
    /// to Laravel's `Inertia::disable_ssr()`. The response falls back
    /// to client-side rendering even when `InertiaConfig::ssr.enabled`
    /// is `true`. Idempotent; no-op outside a request scope.
    pub fn disable_ssr_for_request() {
        crate::inertia::ssr::disable_ssr_for_request();
    }
}

/// Bind a trait to a singleton implementation (auto-wraps in Arc)
///
/// # Example
/// ```rust,no_run
/// # use suprnova::bind;
/// # trait Database: Send + Sync {}
/// # struct PostgresDB;
/// # impl PostgresDB { fn connect(_url: &str) -> Self { PostgresDB } }
/// # impl Database for PostgresDB {}
/// # trait HttpClient: Send + Sync {}
/// # struct RealHttpClient;
/// # impl RealHttpClient { fn new() -> Self { RealHttpClient } }
/// # impl HttpClient for RealHttpClient {}
/// # fn ex(db_url: &str) {
/// bind!(dyn Database, PostgresDB::connect(&db_url));
/// bind!(dyn HttpClient, RealHttpClient::new());
/// # }
/// ```
#[macro_export]
macro_rules! bind {
    ($trait:ty, $instance:expr) => {
        $crate::App::bind::<$trait>(::std::sync::Arc::new($instance) as ::std::sync::Arc<$trait>)
    };
}

/// Bind a trait to a factory (auto-wraps in Arc, new instance each resolution)
///
/// # Example
/// ```rust,no_run
/// # use suprnova::bind_factory;
/// # trait HttpClient: Send + Sync {}
/// # struct RealHttpClient;
/// # impl RealHttpClient { fn new() -> Self { RealHttpClient } }
/// # impl HttpClient for RealHttpClient {}
/// # fn ex() {
/// bind_factory!(dyn HttpClient, || RealHttpClient::new());
/// # }
/// ```
#[macro_export]
macro_rules! bind_factory {
    ($trait:ty, $factory:expr) => {{
        let f = $factory;
        $crate::App::bind_factory::<$trait, _>(move || {
            ::std::sync::Arc::new(f()) as ::std::sync::Arc<$trait>
        })
    }};
}

/// Register a singleton instance (concrete type)
///
/// # Example
/// ```rust,no_run
/// # use suprnova::singleton;
/// # #[derive(Clone)]
/// # struct DatabaseConnection;
/// # impl DatabaseConnection { fn new(_url: &str) -> Self { DatabaseConnection } }
/// # fn ex(url: &str) {
/// singleton!(DatabaseConnection::new(&url));
/// # }
/// ```
#[macro_export]
macro_rules! singleton {
    ($instance:expr) => {
        $crate::App::singleton($instance)
    };
}

/// Register a factory (concrete type, new instance each resolution)
///
/// # Example
/// ```rust,no_run
/// # use suprnova::factory;
/// # struct RequestLogger;
/// # impl RequestLogger { fn new() -> Self { RequestLogger } }
/// # fn ex() {
/// factory!(|| RequestLogger::new());
/// # }
/// ```
#[macro_export]
macro_rules! factory {
    ($factory:expr) => {
        $crate::App::factory($factory)
    };
}

#[cfg(test)]
mod poison_tests {
    //! Lock-poisoning recovery contract for the container.
    //!
    //! `Container` itself is sync (no I/O, just `HashMap` writes); we
    //! can't directly poison the process-global `APP_CONTAINER` without
    //! contaminating every other test in the same process. Instead we
    //! exercise the recover-in-place transformation on a fresh
    //! `RwLock<Container>` that mirrors the production path one-to-one:
    //! `container.write().unwrap_or_else(|e| e.into_inner())` followed
    //! by a mutating call, and the same shape for `read()`.
    //!
    //! These assertions guarantee that the framework keeps registering
    //! services through a poisoned container instead of silently
    //! dropping the binding — the bug fixed alongside these tests.
    use super::*;
    use std::sync::RwLock;
    use std::thread;

    fn poison_container(lock: &Arc<RwLock<Container>>) {
        let clone = Arc::clone(lock);
        let _ = thread::spawn(move || {
            let _g = clone.write().unwrap();
            panic!("intentional poison");
        })
        .join();
        assert!(lock.is_poisoned(), "test setup: lock must be poisoned");
    }

    #[derive(Clone, PartialEq, Debug)]
    struct PoisonProbe(u32);

    #[test]
    fn write_path_registers_through_poison() {
        let lock = Arc::new(RwLock::new(Container::new()));
        poison_container(&lock);

        // Mirror what `App::singleton` does: write through the recover.
        let mut c = lock.write().unwrap_or_else(|e| e.into_inner());
        c.singleton(PoisonProbe(7));
        drop(c);

        // Mirror what `App::get` does: read through the recover.
        let got = lock
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get::<PoisonProbe>();
        assert_eq!(
            got,
            Some(PoisonProbe(7)),
            "registration must survive a poisoned container lock",
        );
    }

    #[test]
    fn bind_path_registers_through_poison() {
        trait Probe: Send + Sync {
            fn id(&self) -> u32;
        }
        struct ProbeImpl(u32);
        impl Probe for ProbeImpl {
            fn id(&self) -> u32 {
                self.0
            }
        }

        let lock = Arc::new(RwLock::new(Container::new()));
        poison_container(&lock);

        let mut c = lock.write().unwrap_or_else(|e| e.into_inner());
        c.bind::<dyn Probe>(Arc::new(ProbeImpl(42)));
        drop(c);

        let got = lock
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .make::<dyn Probe>();
        assert!(
            got.is_some(),
            "trait binding must survive a poisoned container lock",
        );
        assert_eq!(got.unwrap().id(), 42);
    }

    /// The pre-fix `if let Ok(...)` write path silently dropped the
    /// registration on poison — the failure mode the audit flagged.
    /// This regression test pins the pre-fix shape next to the fix so
    /// a future refactor that re-introduces the silent-drop pattern
    /// trips here instead of in production.
    #[test]
    fn legacy_if_let_ok_pattern_silently_drops_on_poison() {
        let lock = Arc::new(RwLock::new(Container::new()));
        poison_container(&lock);

        let mut wrote = false;
        if let Ok(mut c) = lock.write() {
            c.singleton(PoisonProbe(99));
            wrote = true;
        }
        assert!(
            !wrote,
            "the legacy `if let Ok` pattern WOULD silently drop the registration; \
             production now uses `unwrap_or_else(|e| e.into_inner())` instead",
        );

        // And the read side under the same pattern returned None too.
        let read = lock.read().ok().and_then(|c| c.get::<PoisonProbe>());
        assert!(
            read.is_none(),
            "legacy `read().ok()?` returned None on poison"
        );
    }
}

#[cfg(test)]
mod lock_release_tests {
    //! Factory closures must run AFTER the container read lock has been
    //! released. `App::get`/`App::make` clone the binding out from under
    //! the guard via `Container::binding(type_id)` and only then invoke
    //! the factory — otherwise a factory that re-enters `App::*` (or any
    //! writer that needs the lock) would deadlock, and an expensive
    //! factory would needlessly block container mutation.
    //!
    //! The tests below mirror the production extract-drop-resolve path
    //! on a fresh `Arc<RwLock<Container>>` so they don't touch the
    //! process-global `APP_CONTAINER`. `try_write` is used in the probe
    //! so a regression returns `WouldBlock` immediately instead of
    //! hanging the test runner.
    use super::*;
    use std::sync::RwLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Clone, Debug, PartialEq)]
    struct Probe(u32);

    #[test]
    fn factory_runs_without_read_guard_held() {
        let lock = Arc::new(RwLock::new(Container::new()));

        // Probe lock the factory will try to write-lock. If the
        // production path still held a read guard on `lock` during the
        // factory invocation, registering the factory with a closure
        // that probes `lock` itself would be the natural test — but
        // that mixes "binding storage" with "deadlock surface". Use a
        // dedicated probe lock instead so the assertion is unambiguous:
        // the factory must observe that the container lock is free.
        let probe_container = Arc::clone(&lock);
        let saw_lock_free = Arc::new(AtomicBool::new(false));
        let saw_lock_free_in = Arc::clone(&saw_lock_free);

        {
            let mut c = lock.write().unwrap();
            c.factory(move || {
                // While this closure runs we must NOT be holding a read
                // guard on the container — assert by trying to acquire
                // a write guard. `try_write` is non-blocking, so a
                // regression to the old "factory inside read guard"
                // shape returns `WouldBlock` instead of hanging.
                let free = probe_container.try_write().is_ok();
                saw_lock_free_in.store(free, Ordering::SeqCst);
                Probe(123)
            });
        }

        // Mirror the production extract-drop-resolve path one-to-one.
        let type_id = TypeId::of::<Probe>();
        let binding = lock
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .binding(type_id)
            .expect("factory binding must be registered");
        // Lock guard drops at end of previous statement (temporary
        // expression). The binding clone is what we hand to the factory.
        let resolved = binding.resolve_concrete::<Probe>();

        assert_eq!(resolved, Some(Probe(123)));
        assert!(
            saw_lock_free.load(Ordering::SeqCst),
            "factory closure must run AFTER the container read guard is released",
        );
    }

    /// Pin the bad shape: holding the read guard for the entire
    /// resolution (the pre-fix structure) would prevent a concurrent
    /// writer from making progress while the factory is running.
    #[test]
    fn legacy_factory_inside_read_guard_blocks_writers() {
        let lock = Arc::new(RwLock::new(Container::new()));
        let probe_container = Arc::clone(&lock);
        let saw_lock_free = Arc::new(AtomicBool::new(false));
        let saw_lock_free_in = Arc::clone(&saw_lock_free);

        {
            let mut c = lock.write().unwrap();
            c.factory(move || {
                let free = probe_container.try_write().is_ok();
                saw_lock_free_in.store(free, Ordering::SeqCst);
                Probe(7)
            });
        }

        // Pre-fix shape: invoke the factory WHILE the read guard is
        // still alive (the guard is a temporary that lives to the end
        // of the chained call).
        let _resolved = lock
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get::<Probe>();

        assert!(
            !saw_lock_free.load(Ordering::SeqCst),
            "legacy chained `read()...get()` keeps the guard alive while the factory runs — \
             production now extracts the binding first and drops the guard",
        );
    }

    #[test]
    fn make_factory_runs_without_read_guard_held() {
        trait Greeter: Send + Sync {
            fn hello(&self) -> &'static str;
        }
        struct Hi;
        impl Greeter for Hi {
            fn hello(&self) -> &'static str {
                "hi"
            }
        }

        let lock = Arc::new(RwLock::new(Container::new()));
        let probe_container = Arc::clone(&lock);
        let saw_lock_free = Arc::new(AtomicBool::new(false));
        let saw_lock_free_in = Arc::clone(&saw_lock_free);

        {
            let mut c = lock.write().unwrap();
            c.bind_factory::<dyn Greeter, _>(move || {
                let free = probe_container.try_write().is_ok();
                saw_lock_free_in.store(free, Ordering::SeqCst);
                Arc::new(Hi) as Arc<dyn Greeter>
            });
        }

        let type_id = TypeId::of::<Arc<dyn Greeter>>();
        let binding = lock
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .binding(type_id)
            .expect("trait factory binding must be registered");
        let resolved = binding.resolve_make::<dyn Greeter>();

        assert_eq!(resolved.map(|g| g.hello()), Some("hi"));
        assert!(
            saw_lock_free.load(Ordering::SeqCst),
            "trait factory closure must run AFTER the container read guard is released",
        );
    }

    /// `App::flash` must not panic when the value's `Serialize` impl
    /// returns `Err`. Match the sibling `Redirect::with` shape:
    /// log + drop the entry so the rest of the response renders
    /// normally. `HashMap<(i32, i32), &str>` is the canonical
    /// serde-rejected map shape (non-string keys).
    #[test]
    fn flash_does_not_panic_on_serialise_error() {
        use std::collections::HashMap;
        let mut bad: HashMap<(i32, i32), &str> = HashMap::new();
        bad.insert((1, 2), "value");
        // No assertion needed — the test's value is that this call
        // returns normally instead of panicking. Pre-fix it would
        // unwind with `expect("App::flash value must serialize cleanly")`.
        super::App::flash("offending_key", bad);
    }
}
