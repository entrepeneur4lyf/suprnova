//! Phase 9A — [`VectorDriver`] trait + payload structs.

use crate::FrameworkError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One row written to a vector store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorItem {
    pub id: String,
    pub embedding: Vec<f32>,
    /// Free-form JSON shipped alongside the vector. Drivers store
    /// it as `payload`/`metadata` depending on their nomenclature.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl VectorItem {
    pub fn new(
        id: impl Into<String>,
        embedding: Vec<f32>,
        metadata: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            embedding,
            metadata,
        }
    }
}

/// One result returned from a similarity search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorMatch {
    pub id: String,
    /// Driver-specific score — higher is "more similar" by
    /// convention (cosine similarity in the canonical case;
    /// Qdrant's COSINE distance returns the same shape).
    pub score: f32,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Driver contract for a vector backend.
///
/// Implementations must be `Send + Sync + 'static` so they can be
/// registered as `Arc<dyn VectorDriver>` and shared across the
/// app's task pool.
#[async_trait]
pub trait VectorDriver: Send + Sync + 'static {
    /// Insert or update points. Each item's id is the merge key —
    /// an existing point with the same id is replaced.
    async fn upsert(&self, store: &str, items: Vec<VectorItem>) -> Result<(), FrameworkError>;

    /// Return the `k` points most similar to `query`. Order is
    /// best-first.
    async fn similar(
        &self,
        store: &str,
        query: Vec<f32>,
        k: usize,
    ) -> Result<Vec<VectorMatch>, FrameworkError>;

    /// Delete points by id. Unknown ids are ignored (silent
    /// success matches Qdrant / Pinecone semantics).
    async fn delete(&self, store: &str, ids: Vec<String>) -> Result<(), FrameworkError>;

    /// Number of points currently in the store. Used for size
    /// assertions in tests + the [`Vector::registered_names`](crate::Vector::registered_names)
    /// admin path.
    async fn count(&self, store: &str) -> Result<usize, FrameworkError>;
}
