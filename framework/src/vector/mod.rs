//! Phase 9A — Vector store abstraction.
//!
//! Suprnova diverges from Laravel's pgvector-only story by treating
//! every vector backend as a first-class driver behind a single
//! [`VectorDriver`] trait. v1 ships Memory (in-process), Qdrant,
//! Pinecone, and MariaDB (11.7+ native `VECTOR(N)` + HNSW) drivers;
//! Weaviate / Milvus / LanceDB / pgvector / LibSQL queue up behind
//! real consumer demand.
//!
//! ```rust,ignore
//! use suprnova::Vector;
//!
//! let store = Vector::store("documents")?;
//! store.upsert(vec![
//!     VectorItem::new("doc-1", embedding, serde_json::json!({ "title": "Hello" })),
//! ]).await?;
//!
//! let hits = store.similar(query_embedding, 10).await?;
//! ```
//!
//! ## Configuration
//!
//! Drivers are registered at bootstrap via [`Vector::register`].
//! [`Vector::store(name)`](Vector::store) looks the driver up by
//! the store's configured backend. A single app may register
//! multiple drivers — one Qdrant deployment for production
//! semantic search, an in-process Memory driver for tests against
//! the same code path.

pub mod driver;
pub mod mariadb;
pub mod memory;
pub mod pinecone;
pub mod qdrant;
pub mod registry;

pub use driver::{VectorDriver, VectorItem, VectorMatch};
pub use mariadb::{MariaDbDistance, MariaDbVectorDriver};
pub use memory::MemoryVectorDriver;
pub use pinecone::PineconeVectorDriver;
pub use qdrant::{QdrantDistance, QdrantVectorDriver, SUPRNOVA_ID_PAYLOAD_KEY};
pub use registry::{VectorRegistry, VectorStore};

use crate::FrameworkError;
use std::sync::Arc;

/// Static facade — `Vector::store(name)` resolves a store handle,
/// `Vector::register(name, driver)` wires drivers at bootstrap.
pub struct Vector;

impl Vector {
    /// Register a driver for the named store. Subsequent
    /// [`Vector::store`] calls with the same name route through
    /// this driver. Re-registering overwrites (last-wins) to keep
    /// test harnesses well-behaved across runs.
    pub fn register(name: impl Into<String>, driver: Arc<dyn VectorDriver>) {
        VectorRegistry::install(name.into(), driver);
    }

    /// Look up the store handle for `name`. Returns
    /// [`FrameworkError::service_not_found`] if no driver has been
    /// registered under that name.
    pub fn store(name: &str) -> Result<VectorStore, FrameworkError> {
        VectorRegistry::lookup(name)
    }

    /// Names of every registered store. Order is unspecified.
    pub fn registered_names() -> Vec<String> {
        VectorRegistry::names()
    }
}
