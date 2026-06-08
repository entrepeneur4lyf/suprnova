//! Shared-data registry and the [`InertiaSharedData`] trait.
//!
//! The registry lives on the [`crate::container::Container`]; production
//! reads use the global container, tests use the thread-local
//! [`crate::testing::TestContainer`] guard for isolation. There is no
//! process-global static state — set up a test container, register
//! shared data, and the guard cleans it up when dropped.
//!
//! ## Precedence
//!
//! On every Inertia response build, props are layered in this order —
//! later writes overwrite earlier ones at the same key:
//!
//! 1. **Static registry** (sync values + lazy resolvers added via
//!    `App::inertia_share` / `App::inertia_share_lazy`)
//! 2. **Trait registration** (per-request `share(&req)` from the
//!    `InertiaSharedData` provider registered via
//!    `App::register_inertia_shared`)
//! 3. **User-supplied props** attached via the builder

use super::prop::{InertiaRequestExt, OnceConfig, Prop, PropResolver};
use crate::error::FrameworkError;
use crate::lock;
use async_trait::async_trait;
use indexmap::IndexMap;
use serde::Serialize;
use std::future::Future;
use std::sync::{Arc, RwLock};

/// App-level provider of per-request shared data.
///
/// Register a singleton via `App::register_inertia_shared(impl)`.
/// The framework awaits `share(&req)` on every Inertia response and
/// merges the result into the page's props.
#[async_trait]
pub trait InertiaSharedData: Send + Sync + 'static {
    /// Produce the per-request shared props merged into every Inertia response.
    async fn share(
        &self,
        req: &dyn InertiaRequestExt,
    ) -> Result<IndexMap<String, Prop>, FrameworkError>;
}

/// Internal entry in the static shared-data registry.
#[derive(Clone)]
pub(crate) struct StaticEntry {
    pub key: String,
    pub prop: Prop,
}

/// Per-container shared-data registry.
///
/// Lives on `Container::inertia` as an `Arc<InertiaRegistry>`. Methods
/// take `&self` and use interior mutability (`RwLock`) so registrations
/// can happen at any point after the container is constructed without
/// needing `&mut`.
pub struct InertiaRegistry {
    shares: RwLock<Vec<StaticEntry>>,
    provider: RwLock<Option<Arc<dyn InertiaSharedData>>>,
}

impl InertiaRegistry {
    /// Build an empty registry with no static shares and no async provider.
    pub fn new() -> Self {
        Self {
            shares: RwLock::new(Vec::new()),
            provider: RwLock::new(None),
        }
    }

    /// Add or replace a synchronous shared prop. Maps to
    /// `Inertia::share($k, $v)`.
    ///
    /// # Panics
    ///
    /// Panics if `value`'s `Serialize` impl returns `Err`. This is a
    /// programmer error in the value's type — `share_value` is typically
    /// called from `App` bootstrap (`bootstrap.rs`), so a broken
    /// `Serialize` impl surfaces at process startup. If called from a
    /// request handler, the framework's request-level panic-catch
    /// middleware (`framework/src/middleware/chain.rs`) converts the
    /// panic to a 500 instead of taking the process down.
    ///
    /// For runtime-fallible values use [`share_lazy`] — its resolver
    /// returns `Result<V, FrameworkError>` and surfaces serialization
    /// failures as Inertia JSON errors instead of panics.
    ///
    /// [`share_lazy`]: Self::share_lazy
    pub fn share_value<V: Serialize>(&self, key: impl Into<String>, value: V) {
        let v =
            serde_json::to_value(&value).expect("App::inertia_share value must serialize cleanly");
        self.upsert(key.into(), Prop::Eager(v));
    }

    /// Add or replace an async lazy shared prop. Maps to
    /// `Inertia::share($k, fn () => ...)`.
    pub fn share_lazy<F, Fut, V>(&self, key: impl Into<String>, resolver: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<V, FrameworkError>> + Send + 'static,
        V: Serialize + 'static,
    {
        let resolver = make_resolver(resolver);
        self.upsert(key.into(), Prop::Lazy(resolver));
    }

    /// Add or replace a shared *once* prop. The resolver runs once when
    /// the client doesn't already have the cache entry, then the client
    /// remembers the value across navigations (signaled via
    /// `X-Inertia-Except-Once-Props`). Maps to `Inertia::shareOnce(...)`.
    pub fn share_once<F, Fut, V>(&self, key: impl Into<String>, resolver: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<V, FrameworkError>> + Send + 'static,
        V: Serialize + 'static,
    {
        let resolver = make_resolver(resolver);
        let key = key.into();
        let cache_key = key.clone();
        self.upsert(
            key,
            Prop::Once(OnceConfig {
                resolver,
                cache_key,
                expires_at: None,
                fresh: false,
            }),
        );
    }

    fn upsert(&self, key: String, prop: Prop) {
        // Poison policy (Domain 20 audit D20-A): if the registry lock is
        // poisoned the upsert is skipped and a `tracing::error!` is logged.
        // The framework's lock helper already returns Result; defeating it
        // with `.expect(...)` would let a single panic cascade through
        // every subsequent `App::inertia_share*` call. Read side
        // (`snapshot_static`/`trait_provider`) already propagates via `?`,
        // so the asymmetry was visible.
        match lock::write(&self.shares, "inertia share registry") {
            Ok(mut reg) => {
                if let Some(existing) = reg.iter_mut().find(|e| e.key == key) {
                    existing.prop = prop;
                } else {
                    reg.push(StaticEntry { key, prop });
                }
            }
            Err(_) => {
                tracing::error!(
                    %key,
                    "Inertia share registry lock poisoned; skipping upsert."
                );
            }
        }
    }

    /// Register the singleton [`InertiaSharedData`] implementation.
    /// Subsequent calls replace any prior registration.
    ///
    /// **Poison policy** (Domain 20 audit D20-A): on lock poison the
    /// registration is skipped and a `tracing::error!` is emitted.
    pub fn register_trait(&self, provider: Arc<dyn InertiaSharedData>) {
        match lock::write(&self.provider, "inertia shared trait slot") {
            Ok(mut slot) => {
                *slot = Some(provider);
            }
            Err(_) => {
                tracing::error!("Inertia shared trait slot lock poisoned; skipping registration.");
            }
        }
    }

    /// Snapshot of the static registry — clones each entry. Cheap because
    /// `Prop` either holds a `Value` (cheap clone) or an `Arc`-backed
    /// resolver. Internal use by `InertiaResponse::resolve`.
    pub(crate) fn snapshot_static(&self) -> Result<Vec<(String, Prop)>, FrameworkError> {
        let reg = lock::read(&self.shares, "inertia share registry")?;
        Ok(reg
            .iter()
            .map(|e| (e.key.clone(), e.prop.clone()))
            .collect())
    }

    /// Currently registered trait provider, if any. Internal use.
    pub(crate) fn trait_provider(
        &self,
    ) -> Result<Option<Arc<dyn InertiaSharedData>>, FrameworkError> {
        Ok(lock::read(&self.provider, "inertia shared trait slot")?.clone())
    }
}

impl Default for InertiaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn make_resolver<F, Fut, V>(resolver: F) -> PropResolver
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<V, FrameworkError>> + Send + 'static,
    V: Serialize + 'static,
{
    Arc::new(move || {
        let fut = resolver();
        Box::pin(async move {
            let value = fut.await?;
            serde_json::to_value(&value).map_err(|e| {
                FrameworkError::internal(format!(
                    "Inertia lazy shared prop failed to serialize: {}",
                    e
                ))
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn share_value_inserts() {
        let reg = InertiaRegistry::new();
        reg.share_value("appName", "Suprnova");
        let snap = reg.snapshot_static().unwrap();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].0, "appName");
        match &snap[0].1 {
            Prop::Eager(v) => assert_eq!(v, &Value::String("Suprnova".into())),
            _ => panic!("expected eager prop"),
        }
    }

    #[test]
    fn upsert_replaces_existing_key() {
        let reg = InertiaRegistry::new();
        reg.share_value("k", "v1");
        reg.share_value("k", "v2");
        let snap = reg.snapshot_static().unwrap();
        assert_eq!(snap.len(), 1);
        match &snap[0].1 {
            Prop::Eager(v) => assert_eq!(v, &Value::String("v2".into())),
            _ => panic!("expected eager prop"),
        }
    }

    #[tokio::test]
    async fn share_lazy_resolver_runs_when_resolved() {
        let reg = InertiaRegistry::new();
        reg.share_lazy("count", || async { Ok::<_, FrameworkError>(42u32) });
        let snap = reg.snapshot_static().unwrap();
        match snap[0].1.clone() {
            Prop::Lazy(r) => {
                let v = r().await.unwrap();
                assert_eq!(v, Value::Number(42.into()));
            }
            _ => panic!("expected lazy prop"),
        }
    }

    #[tokio::test]
    async fn trait_provider_round_trip() {
        let reg = InertiaRegistry::new();

        struct Prov;
        #[async_trait]
        impl InertiaSharedData for Prov {
            async fn share(
                &self,
                _req: &dyn InertiaRequestExt,
            ) -> Result<IndexMap<String, Prop>, FrameworkError> {
                let mut m = IndexMap::new();
                m.insert(
                    "auth".to_string(),
                    Prop::Eager(Value::String("alice".into())),
                );
                Ok(m)
            }
        }

        reg.register_trait(Arc::new(Prov));

        struct DummyReq;
        impl InertiaRequestExt for DummyReq {
            fn path(&self) -> &str {
                "/"
            }
            fn header(&self, _: &str) -> Option<&str> {
                None
            }
        }

        let provider = reg.trait_provider().unwrap().unwrap();
        let shared = provider.share(&DummyReq).await.unwrap();
        assert_eq!(shared.len(), 1);
        assert!(shared.contains_key("auth"));
    }

    #[test]
    fn separate_registries_are_isolated() {
        let r1 = InertiaRegistry::new();
        let r2 = InertiaRegistry::new();
        r1.share_value("only_in_r1", "x");
        assert_eq!(r1.snapshot_static().unwrap().len(), 1);
        assert!(r2.snapshot_static().unwrap().is_empty());
    }
}
