//! Phase 9A — [`VectorDriver`] trait + payload structs.

use crate::FrameworkError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One row written to a vector store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorItem {
    /// Caller-chosen merge key. Upserts replace any existing row with the same id.
    pub id: String,
    /// Vector embedding; length must match the store's configured dimension.
    pub embedding: Vec<f32>,
    /// Free-form JSON shipped alongside the vector. Drivers store
    /// it as `payload`/`metadata` depending on their nomenclature.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl VectorItem {
    /// Construct a [`VectorItem`] from its three components.
    pub fn new(id: impl Into<String>, embedding: Vec<f32>, metadata: serde_json::Value) -> Self {
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
    /// Id of the matched item (the same id supplied at upsert time).
    pub id: String,
    /// Driver-specific similarity score. **Interpretation depends on
    /// the backend's distance metric**:
    ///
    /// - **Cosine / dot-product** (canonical case, Pinecone / Qdrant
    ///   Cosine / Memory) — higher is more similar (range usually
    ///   `[-1, 1]` for cosine, unbounded for dot).
    /// - **Qdrant Euclid / Manhattan / Chebyshev** — these are
    ///   distance metrics; Qdrant returns the raw distance, so
    ///   **lower is more similar**. `0.0` means identical.
    ///
    /// The driver's docstring lists the metric it ships with; if you
    /// switch to a distance metric at index-creation time and rely on
    /// `score` ordering downstream, flip the comparison accordingly.
    /// `similar()` always sorts best-first per the driver's own
    /// definition of "best", so for typical "find top-k similar"
    /// usage the field's direction is transparent.
    pub score: f32,
    /// Metadata persisted alongside the vector at upsert time.
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
