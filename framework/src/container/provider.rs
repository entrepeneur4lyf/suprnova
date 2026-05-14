//! Service auto-registration for suprnova framework
//!
//! This module provides automatic service registration via macros:
//! - `#[service(ConcreteType)]` - auto-register trait bindings
//! - `#[derive(Injectable)]` - auto-register concrete types as singletons
//!
//! # Example - Trait binding
//!
//! ```rust,ignore
//! use suprnova::service;
//!
//! // Auto-register: dyn CacheStore → RedisCache
//! #[service(RedisCache)]
//! pub trait CacheStore: Send + Sync + 'static {
//!     fn get(&self, key: &str) -> Option<String>;
//!     fn set(&self, key: &str, value: &str);
//! }
//!
//! pub struct RedisCache;
//! impl Default for RedisCache {
//!     fn default() -> Self { Self }
//! }
//! impl CacheStore for RedisCache { ... }
//! ```
//!
//! # Example - Concrete singleton
//!
//! ```rust,ignore
//! use suprnova::injectable;
//!
//! #[injectable]
//! pub struct AppState {
//!     pub counter: u32,
//! }
//!
//! // Resolve via:
//! let state: AppState = App::get().unwrap();
//! ```

/// Entry for inventory-collected service bindings (trait → impl)
///
/// Used internally by the `#[service(ConcreteType)]` macro to register
/// service bindings at compile time.
pub struct ServiceBindingEntry {
    /// Function to register the service binding
    pub register: fn(),
    /// Service name for debugging/logging
    pub name: &'static str,
}

/// Entry for inventory-collected singleton registrations (concrete types)
///
/// Used internally by the `#[derive(Injectable)]` macro to register
/// concrete singletons at compile time.
pub struct SingletonEntry {
    /// Function to register the singleton
    pub register: fn(),
    /// Type name for debugging/logging
    pub name: &'static str,
}

// Inventory collection for auto-registered service bindings
inventory::collect!(ServiceBindingEntry);

// Inventory collection for auto-registered singletons
inventory::collect!(SingletonEntry);

/// Register all service bindings from inventory
///
/// This is called automatically by `Server::from_config()`.
/// It registers all services marked with `#[service(ConcreteType)]`.
pub fn register_service_bindings() {
    for entry in inventory::iter::<ServiceBindingEntry> {
        (entry.register)();
    }
}

/// Register all singleton entries from inventory
///
/// This is called automatically by `Server::from_config()`.
/// It registers all types marked with `#[derive(Injectable)]`.
pub fn register_singletons() {
    for entry in inventory::iter::<SingletonEntry> {
        (entry.register)();
    }
}

/// Full bootstrap sequence for services
///
/// Called automatically by `Server::from_config()`.
pub fn bootstrap() {
    register_service_bindings();
    register_singletons();
}
