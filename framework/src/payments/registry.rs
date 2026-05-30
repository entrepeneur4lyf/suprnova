//! Process-global payment provider registry.
//!
//! Two registration mechanisms are supported:
//!
//! 1. **Compile-time** — via `inventory::submit!(PaymentProviderEntry { ... })`. Entries are
//!    collected at link time. This is the recommended mechanism for driver crates that want
//!    zero-config registration.
//!
//! 2. **Runtime** — via `PaymentProviderRegistry::bind(name, provider)`. Used by tests and by
//!    apps that construct providers with runtime config (API keys from environment variables).
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::payments::{PaymentProviderRegistry, MockPaymentProvider};
//! use std::sync::Arc;
//!
//! PaymentProviderRegistry::bind("stripe", Arc::new(my_stripe_provider));
//! let provider = PaymentProviderRegistry::get("stripe").expect("stripe not registered");
//! ```

use crate::payments::PaymentProvider;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

/// A compile-time registry entry for a payment provider.
///
/// Submitted via `inventory::submit!` — typically at the bottom of the file
/// that defines the [`PaymentProvider`] implementation:
///
/// ```rust,ignore
/// inventory::submit!(PaymentProviderEntry {
///     name: "stripe",
///     factory: || Arc::new(StripeProvider::new()),
/// });
/// ```
pub struct PaymentProviderEntry {
    /// Stable kebab-case name that matches the provider's `PaymentProvider::name()` return value.
    pub name: &'static str,
    /// Factory function that constructs a new instance of the provider.
    /// Called once at registry initialization.
    pub factory: fn() -> Arc<dyn PaymentProvider>,
}

inventory::collect!(PaymentProviderEntry);

static REGISTRY: OnceLock<RwLock<HashMap<&'static str, Arc<dyn PaymentProvider>>>> =
    OnceLock::new();

fn ensure_built() -> &'static RwLock<HashMap<&'static str, Arc<dyn PaymentProvider>>> {
    REGISTRY.get_or_init(|| {
        let mut map = HashMap::new();
        for entry in inventory::iter::<PaymentProviderEntry> {
            map.insert(entry.name, (entry.factory)());
        }
        RwLock::new(map)
    })
}

/// Process-global registry of [`PaymentProvider`] instances.
///
/// Providers registered via `inventory::submit!(PaymentProviderEntry { ... })` are collected
/// automatically at startup. Additional providers can be registered at runtime via [`bind`].
///
/// [`bind`]: PaymentProviderRegistry::bind
pub struct PaymentProviderRegistry;

impl PaymentProviderRegistry {
    /// Look up a provider by name. Returns `None` if no provider with that name is
    /// registered.
    ///
    /// **Poison policy** (Domain 19 audit D19-A): if the registry lock is poisoned
    /// the lookup returns `None` and a `tracing::error!` is emitted. Callers see
    /// the same "not registered" shape as a missing entry, which keeps a single
    /// panic from cascading through every subsequent payment call.
    pub fn get(name: &str) -> Option<Arc<dyn PaymentProvider>> {
        match crate::lock::read(ensure_built(), "payments registry") {
            Ok(map) => map.get(name).cloned(),
            Err(_) => {
                tracing::error!(
                    provider = %name,
                    "Payments registry lock poisoned; treating lookup as miss."
                );
                None
            }
        }
    }

    /// Snapshot of registered provider names. Order is unspecified.
    ///
    /// On lock poison returns an empty vec and logs at `error` level.
    pub fn names() -> Vec<&'static str> {
        match crate::lock::read(ensure_built(), "payments registry") {
            Ok(map) => map.keys().copied().collect(),
            Err(_) => {
                tracing::error!("Payments registry lock poisoned; returning empty names list.");
                Vec::new()
            }
        }
    }

    /// Bind a provider at runtime, bypassing the inventory mechanism.
    ///
    /// Used by tests and by apps that want to construct providers with runtime
    /// config (e.g. API keys from environment variables). Overwrites any
    /// previously registered provider with the same name.
    ///
    /// On lock poison the bind is skipped and a `tracing::error!` is emitted —
    /// next `get()` call will then miss, matching the read-path policy.
    pub fn bind(name: &'static str, provider: Arc<dyn PaymentProvider>) {
        match crate::lock::write(ensure_built(), "payments registry") {
            Ok(mut map) => {
                map.insert(name, provider);
            }
            Err(_) => {
                tracing::error!(
                    provider = %name,
                    "Payments registry lock poisoned; skipping provider bind."
                );
            }
        }
    }
}
