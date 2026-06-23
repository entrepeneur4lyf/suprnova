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
//! ```rust,no_run
//! use suprnova::{injectable, App};
//!
//! #[injectable]
//! pub struct AppState {
//!     pub counter: u32,
//! }
//!
//! # fn ex() {
//! // Resolve via:
//! let state: AppState = App::get().unwrap();
//! # }
//! ```
//!
//! # Boot order and dependency resolution
//!
//! Service bindings (`#[service(...)]`) install a default-constructed concrete
//! impl for a trait; they never reach into the container themselves, so they
//! can register in any order with no inter-dependencies. They are processed
//! first as a single pass.
//!
//! Singletons (`#[injectable]`) may declare dependencies via `#[inject]`,
//! pulling them out of the container as they construct. Inventory iteration
//! order is implementation-defined, so a naive single pass could try to
//! resolve a dependency before the producer was registered and report a
//! spurious "missing service" failure. To survive that, [`bootstrap`] runs the
//! singleton entries in a fixed-point loop: every iteration tries every
//! still-pending entry once, removes the ones that succeeded, and stops when
//! either the pending set is empty (success) or a full pass made zero progress
//! (genuine missing or cyclic dependency — returned as a structured error
//! naming the failing entry).

use crate::error::FrameworkError;

/// Entry for inventory-collected service bindings (trait → impl)
///
/// Used internally by the `#[service(ConcreteType)]` macro to register
/// service bindings at compile time. The register function returns
/// `Result<(), String>` so a registration that depends on the container
/// (e.g. via `App::resolve`) can report a missing dependency instead of
/// panicking.
pub struct ServiceBindingEntry {
    /// Function to register the service binding. Returns `Ok(())` on success
    /// or `Err(reason)` if the binding cannot be installed yet (e.g. a
    /// transitive `App::resolve` failed).
    pub register: fn() -> Result<(), String>,
    /// Service name for debugging/logging
    pub name: &'static str,
}

/// Entry for inventory-collected singleton registrations (concrete types)
///
/// Used internally by the `#[derive(Injectable)]` macro to register
/// concrete singletons at compile time. The register function returns
/// `Result<(), String>` so a singleton with `#[inject]` fields can report
/// a missing dependency rather than panicking; the bootstrap loop retries
/// failed entries until they all succeed or progress stalls.
pub struct SingletonEntry {
    /// Function to register the singleton. Returns `Ok(())` on success or
    /// `Err(reason)` if a dependency wasn't registered yet — the bootstrap
    /// loop will retry on later iterations.
    pub register: fn() -> Result<(), String>,
    /// Type name for debugging/logging
    pub name: &'static str,
}

// Inventory collection for auto-registered service bindings
inventory::collect!(ServiceBindingEntry);

// Inventory collection for auto-registered singletons
inventory::collect!(SingletonEntry);

/// Register all service bindings from inventory.
///
/// Services have no inter-service dependencies (each just installs a
/// `Default::default()` concrete impl), so a single pass is sufficient.
/// Any error is wrapped into a `FrameworkError::internal` naming the
/// failing entry and returned immediately.
pub fn register_service_bindings() -> Result<(), FrameworkError> {
    for entry in inventory::iter::<ServiceBindingEntry> {
        (entry.register)().map_err(|reason| {
            FrameworkError::internal(format!(
                "service `{}` failed to register: {reason}",
                entry.name
            ))
        })?;
    }
    Ok(())
}

/// Register all singleton entries from inventory.
///
/// Singletons can declare `#[inject]` dependencies on other singletons.
/// Inventory order is implementation-defined, so we run a fixed-point loop:
/// each iteration tries every pending entry; entries that succeed drop out
/// of the pending set; the loop stops when either the set empties (success)
/// or a full pass makes no progress (return Err naming the most recently
/// failing entry — its `reason` typically already says which transitive
/// dependency couldn't be resolved).
pub fn register_singletons() -> Result<(), FrameworkError> {
    // Snapshot inventory into an owned vec so we can drain it across
    // multiple passes without reborrowing the iterator each round.
    let mut pending: Vec<&'static SingletonEntry> =
        inventory::iter::<SingletonEntry>.into_iter().collect();

    if pending.is_empty() {
        return Ok(());
    }

    loop {
        let before = pending.len();
        let mut next_pending: Vec<&'static SingletonEntry> = Vec::with_capacity(before);
        let mut iteration_failures: Vec<(&'static str, String)> = Vec::new();

        for entry in pending.drain(..) {
            match (entry.register)() {
                Ok(()) => {
                    // Registered (or already present via if_absent) — done.
                }
                Err(reason) => {
                    iteration_failures.push((entry.name, reason));
                    next_pending.push(entry);
                }
            }
        }

        if next_pending.is_empty() {
            // All entries succeeded this iteration — done.
            return Ok(());
        }

        if next_pending.len() == before {
            // No progress this iteration: the remaining entries cannot be
            // registered because their dependencies are missing or form a
            // cycle. Report the first failure from this iteration; its
            // `reason` text typically names the unresolved type.
            let (name, reason) = iteration_failures
                .into_iter()
                .next()
                .unwrap_or_else(|| ("<unknown>", "no progress in singleton boot loop".into()));
            return Err(FrameworkError::internal(format!(
                "singleton `{name}` could not be booted: {reason} \
                 (no progress across the remaining {} entries — check for \
                 a missing #[injectable] type or a cyclic dependency)",
                next_pending.len()
            )));
        }

        // Made progress; drop the iteration's failure list and try again.
        // The next iteration will repopulate it for any entries that still
        // can't resolve.
        let _ = iteration_failures;
        pending = next_pending;
    }
}

/// Full bootstrap sequence for services.
///
/// Called automatically by `Server::from_config()`. Services first, then the
/// fixed-point loop for singletons. Any failure is returned as a structured
/// `FrameworkError::internal` naming the failing entry — `Server::from_config`
/// propagates it as the boot error.
pub fn bootstrap() -> Result<(), FrameworkError> {
    register_service_bindings()?;
    register_singletons()?;
    Ok(())
}
