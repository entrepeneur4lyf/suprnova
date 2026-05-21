//! Phase 9A — process-global driver registry + the public
//! [`VectorStore`] handle.

use super::driver::{VectorDriver, VectorItem, VectorMatch};
use crate::FrameworkError;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

static REGISTRY: OnceLock<RwLock<HashMap<String, Arc<dyn VectorDriver>>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<String, Arc<dyn VectorDriver>>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Static facade for the registry — call sites usually go through
/// [`Vector::register`](super::Vector::register) /
/// [`Vector::store`](super::Vector::store) but this is exposed for
/// integration tests that need to clear state between runs.
pub struct VectorRegistry;

impl VectorRegistry {
    /// Insert or replace a driver under `name`.
    pub fn install(name: String, driver: Arc<dyn VectorDriver>) {
        registry()
            .write()
            .expect("vector registry lock not poisoned")
            .insert(name, driver);
    }

    /// Resolve a [`VectorStore`] handle by name.
    pub fn lookup(name: &str) -> Result<VectorStore, FrameworkError> {
        let driver = registry()
            .read()
            .map_err(|_| FrameworkError::internal("vector registry lock poisoned"))?
            .get(name)
            .cloned()
            .ok_or_else(|| {
                FrameworkError::not_found(format!(
                    "vector store '{name}' is not registered — call \
                     Vector::register(\"{name}\", driver) at bootstrap"
                ))
            })?;
        Ok(VectorStore {
            name: name.to_string(),
            driver,
        })
    }

    /// Snapshot of registered store names. Order is unspecified.
    pub fn names() -> Vec<String> {
        registry()
            .read()
            .map(|r| r.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Drop every registered driver. Intended for tests that need
    /// hermetic state — production code never calls this.
    #[doc(hidden)]
    pub fn clear() {
        if let Ok(mut guard) = registry().write() {
            guard.clear();
        }
    }
}

/// Handle to a named vector store. Constructed by
/// [`Vector::store`](super::Vector::store) — never directly.
#[derive(Clone)]
pub struct VectorStore {
    name: String,
    driver: Arc<dyn VectorDriver>,
}

impl VectorStore {
    /// Configured store name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Insert or update points.
    pub async fn upsert(&self, items: Vec<VectorItem>) -> Result<(), FrameworkError> {
        self.driver.upsert(&self.name, items).await
    }

    /// Top-`k` similarity search.
    pub async fn similar(
        &self,
        query: Vec<f32>,
        k: usize,
    ) -> Result<Vec<VectorMatch>, FrameworkError> {
        self.driver.similar(&self.name, query, k).await
    }

    /// Delete points by id.
    pub async fn delete(
        &self,
        ids: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<(), FrameworkError> {
        let ids: Vec<String> = ids.into_iter().map(Into::into).collect();
        self.driver.delete(&self.name, ids).await
    }

    /// Number of points currently in the store.
    pub async fn count(&self) -> Result<usize, FrameworkError> {
        self.driver.count(&self.name).await
    }
}
