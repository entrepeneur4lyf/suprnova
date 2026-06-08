//! Phase 9A — in-process [`VectorDriver`] backed by a `HashMap`.
//!
//! Used by unit tests and the `dev` profile when an external
//! vector backend isn't worth spinning up. Cosine similarity is
//! the only ranking — drivers that natively offer dot-product /
//! Euclidean shapes (Qdrant / Pinecone) expose those in their
//! own configuration, not here.

use super::driver::{VectorDriver, VectorItem, VectorMatch};
use crate::FrameworkError;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

/// In-process vector driver. Cheap to construct, hermetic per
/// instance (no shared state between two `MemoryVectorDriver::new()`
/// calls).
#[derive(Default)]
pub struct MemoryVectorDriver {
    // Map: store name -> point id -> VectorItem.
    stores: RwLock<HashMap<String, HashMap<String, VectorItem>>>,
}

impl MemoryVectorDriver {
    /// Construct an empty in-memory vector store. Equivalent to
    /// [`MemoryVectorDriver::default`].
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl VectorDriver for MemoryVectorDriver {
    async fn upsert(&self, store: &str, items: Vec<VectorItem>) -> Result<(), FrameworkError> {
        let mut guard = self
            .stores
            .write()
            .map_err(|_| FrameworkError::internal("memory vector lock poisoned"))?;
        let entry = guard.entry(store.to_string()).or_default();
        for item in items {
            entry.insert(item.id.clone(), item);
        }
        Ok(())
    }

    async fn similar(
        &self,
        store: &str,
        query: Vec<f32>,
        k: usize,
    ) -> Result<Vec<VectorMatch>, FrameworkError> {
        let guard = self
            .stores
            .read()
            .map_err(|_| FrameworkError::internal("memory vector lock poisoned"))?;
        let bucket = match guard.get(store) {
            Some(b) => b,
            None => return Ok(Vec::new()),
        };
        let q_norm = norm(&query);
        if q_norm == 0.0 {
            return Err(FrameworkError::param(
                "vector::similar query is zero-vector",
            ));
        }
        let mut scored: Vec<VectorMatch> = bucket
            .values()
            .filter_map(|item| {
                if item.embedding.len() != query.len() {
                    return None;
                }
                let score = cosine(&query, &item.embedding, q_norm);
                Some(VectorMatch {
                    id: item.id.clone(),
                    score,
                    metadata: item.metadata.clone(),
                })
            })
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(k);
        Ok(scored)
    }

    async fn delete(&self, store: &str, ids: Vec<String>) -> Result<(), FrameworkError> {
        let mut guard = self
            .stores
            .write()
            .map_err(|_| FrameworkError::internal("memory vector lock poisoned"))?;
        if let Some(bucket) = guard.get_mut(store) {
            for id in ids {
                bucket.remove(&id);
            }
        }
        Ok(())
    }

    async fn count(&self, store: &str) -> Result<usize, FrameworkError> {
        let guard = self
            .stores
            .read()
            .map_err(|_| FrameworkError::internal("memory vector lock poisoned"))?;
        Ok(guard.get(store).map(|b| b.len()).unwrap_or(0))
    }
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

fn cosine(query: &[f32], item: &[f32], q_norm: f32) -> f32 {
    let dot: f32 = query.iter().zip(item.iter()).map(|(a, b)| a * b).sum();
    let item_norm = norm(item);
    if item_norm == 0.0 {
        return 0.0;
    }
    dot / (q_norm * item_norm)
}
