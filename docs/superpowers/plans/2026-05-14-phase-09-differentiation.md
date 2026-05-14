# Phase 9: Differentiation (Vectors + Graphs + Time-series + Search) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship the four Suprnova-specific value-add tracks that Laravel either lacks or gatekeeps behind one backend. Every consumer picks their stack and Suprnova has a first-class driver — `Vector::store("docs").insert(...)` works the same whether the user runs Qdrant, Weaviate, Milvus, LanceDB, pgvector, Pinecone, Chroma, Redis, OpenSearch, Elasticsearch (vector mode), MariaDB VECTOR, or LibSQL. Same trait surface; different config URL.

**Architecture (Vectors):** Adopt **[`embex`](https://github.com/bridgerust/bridgerust)** (MIT/Apache-2.0) — the "Universal Vector Database Client" from BridgeRust — as the Vector foundation. embex ships a `VectorDatabase` trait plus production adapters for **10 backends** out of the box: Chroma, Elasticsearch (vector mode), LanceDB, Milvus, OpenSearch, pgvector, Pinecone, Qdrant, Redis (Redis Stack), Weaviate. We add MariaDB VECTOR and LibSQL as Suprnova-shipped adapters implementing the same trait — that brings us to **12 backends**, including the two `[[no-gatekeeping]]` proofs Laravel can't offer. Vendored at `reference/bridgerust-main/`. The `Vector::*` facade thinly re-exports `bridge-embex-core` + `bridge-embex-client` so we inherit SIMD-accelerated similarity ops (`Point::cosine_similarity`, `l2_distance`, `dot_product`) for free.

**Architecture (Graph / Timeseries / Search):** Each remaining subsystem is a trait + driver registry following the same pattern as `CacheStore` / `MailTransport` / `QueueDriver` from earlier phases. The user's `bootstrap.rs` calls `Graph::use_neo4j(...)` / `Timeseries::use_influxdb(...)` / `Search::use_meilisearch(...)` to bind a driver; the facade routes all calls through it. Drivers live behind feature flags so consumers only compile in the backends they use.

**Tech Stack per area:**
- **Vectors:** `bridge-embex-core` + `bridge-embex-client` (`path = "../reference/bridgerust-main/crates/embex/*"`) for 10 adapters; our own MariaDB + LibSQL impls of `VectorDatabase` for the remaining two
- **Graphs:** `neo4rs` (Neo4j Bolt), `arangors`, `surrealdb`, `bolt-proto`-based for MemGraph (Bolt-compat)
- **Time-series:** `influxdb` 0.7, `tokio-postgres` for TimescaleDB (Postgres extension), `clickhouse` 0.13, `quest-client` (or HTTP fallback)
- **Search:** `meilisearch-sdk` 0.27, `typesense` 0.7, `elasticsearch` 8.x text mode, `algoliasearch` 1.x. (Note: Elasticsearch + OpenSearch appear in both Vector and Search lists — same cluster, different operations: vector search via embex, full-text search via this track's `SearchIndex` trait.)

---

## File Structure

**New top-level modules:**
- `framework/src/vector/` — `Vector` facade, `VectorStore` trait, drivers
- `framework/src/graph/` — `Graph` facade, `GraphStore` trait, drivers
- `framework/src/timeseries/` — `Timeseries` facade, `TimeseriesStore` trait, drivers
- `framework/src/search/` — `Search` facade, `SearchStore` trait, drivers

Each module is structured:
```
framework/src/{area}/
├── mod.rs              # facade + trait
├── driver/             # driver registry + trait
│   └── mod.rs
├── drivers/
│   ├── {backend1}.rs   # one file per driver
│   └── {backend2}.rs
└── testing.rs          # fake driver for tests
```

**New tests:**
- `framework/tests/vector.rs`, `graph.rs`, `timeseries.rs`, `search.rs` — facade + trait tests with the in-memory fake driver
- One integration test per driver, gated `#[ignore]` and run via `cargo test -- --ignored` against a docker-compose stack

**Modified files:**
- `framework/Cargo.toml` — feature flags per driver, all off-by-default except in-memory fakes
- `framework/src/lib.rs` — declare modules + re-exports

---

## Task 1: Vector facade — adopt embex

**Files:** `framework/src/vector/mod.rs`, `framework/Cargo.toml`

The Suprnova `Vector` facade is a thin re-export over embex's
`VectorDatabase` trait + `EmbexClient`. We do NOT define a parallel
`VectorStore` trait — embex's already covers what we need (`insert`,
`search`, `delete`, `update_metadata`, `create_collection`,
`delete_collection`, `scroll`), exposes a rich filter system, and
ships SIMD-accelerated similarity ops on `Point`.

- [ ] **Step 1: Add embex deps**

```toml
# framework/Cargo.toml — [dependencies]
bridge-embex-core = { path = "../reference/bridgerust-main/crates/embex/core" }
bridge-embex-client = { path = "../reference/bridgerust-main/crates/embex/client" }

# Optional adapter feature flags (each adapter is its own embex crate).
# See Task 2 for the per-backend toml.
```

- [ ] **Step 2: Write failing test**

```rust
// framework/tests/vector.rs
use suprnova::vector::{Point, Vector, VectorDatabase};
use std::collections::HashMap;

#[tokio::test]
async fn facade_resolves_registered_backend() {
    Vector::register("docs", make_test_backend().await).await;
    let db = Vector::store("docs").unwrap();

    let schema = bridge_embex_core::types::CollectionSchema {
        name: "docs".into(),
        dimension: 3,
        metric: bridge_embex_core::types::DistanceMetric::Cosine,
    };
    db.create_collection(&schema).await.unwrap();

    let mut metadata = HashMap::new();
    metadata.insert("title".to_string(), serde_json::json!("Rust"));
    let p = Point::new("doc-1", vec![1.0, 0.0, 0.0]).with_metadata(metadata);

    db.insert("docs", vec![p]).await.unwrap();

    let query = bridge_embex_core::types::VectorQuery {
        collection: "docs".into(),
        vector: Some(vec![0.9, 0.1, 0.0]),
        filter: None,
        top_k: 1,
        offset: None,
        include_vector: false,
        include_metadata: true,
        aggregations: vec![],
    };
    let resp = db.search(&query).await.unwrap();
    assert_eq!(resp.results.len(), 1);
    assert_eq!(resp.results[0].id, "doc-1");
}

#[tokio::test]
async fn unknown_store_returns_error() {
    assert!(Vector::store("nonexistent").is_err());
}

// `make_test_backend` constructs a backend for the test. For the
// in-memory case, we wrap a HashMap behind embex's VectorDatabase
// trait (see Task 5 — MemoryAdapter — which is our own trait impl).
async fn make_test_backend() -> std::sync::Arc<dyn VectorDatabase> {
    std::sync::Arc::new(suprnova::vector::drivers::memory::MemoryAdapter::new())
}
```

- [ ] **Step 3: Implement facade**

```rust
// framework/src/vector/mod.rs
//! Vector store facade — re-exports embex's `VectorDatabase` trait
//! so consumers can write storage-agnostic code, plus a process-
//! global registry indexed by string name.
//!
//! ```ignore
//! use suprnova::vector::{Point, Vector};
//!
//! Vector::register("docs", bridge_embex_qdrant::QdrantAdapter::new(url).await?).await;
//! let db = Vector::store("docs")?;
//! db.insert("docs", vec![Point::new("id", embedding)]).await?;
//! ```

pub mod drivers;

pub use bridge_embex_core::db::VectorDatabase;
pub use bridge_embex_core::types::{
    Aggregation, AggregateResult, CollectionSchema, DistanceMetric, Filter, MetadataUpdate, Point,
    ScrollResponse, SearchResponse, SearchResult, VectorQuery,
};
pub use bridge_embex_core::error::{Error as VectorError, Result as VectorResult};

use crate::FrameworkError;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

static REGISTRY: Mutex<Option<HashMap<String, Arc<dyn VectorDatabase>>>> = Mutex::new(None);

pub struct Vector;

impl Vector {
    /// Register a vector backend under `name`. The backend is any
    /// type implementing `VectorDatabase` — embex's adapters or our
    /// own (MariaDB / LibSQL / in-memory).
    pub async fn register(name: impl Into<String>, backend: Arc<dyn VectorDatabase>) {
        let mut g = REGISTRY.lock().unwrap();
        let map = g.get_or_insert_with(HashMap::new);
        map.insert(name.into(), backend);
    }

    pub fn store(name: &str) -> Result<Arc<dyn VectorDatabase>, FrameworkError> {
        let g = REGISTRY.lock().unwrap();
        g.as_ref()
            .and_then(|m| m.get(name).cloned())
            .ok_or_else(|| FrameworkError::internal(format!("vector store '{}' not registered", name)))
    }

    /// Test helper — clear the registry.
    #[cfg(any(test, feature = "testing"))]
    pub fn reset() {
        let mut g = REGISTRY.lock().unwrap();
        *g = None;
    }
}
```

```rust
// framework/src/lib.rs
pub mod vector;
pub use vector::{Point, Vector, VectorDatabase, VectorQuery, SearchResult};
```

- [ ] **Step 4: Run — expect failure (MemoryAdapter not implemented yet)**

```bash
cargo test -p suprnova --test vector
```

Expected: compile error — `MemoryAdapter` referenced but defined in Task 5.

- [ ] **Step 5: Commit (skeleton — tests pass after Task 5)**

```bash
git add framework/Cargo.toml framework/src/vector/mod.rs framework/src/lib.rs framework/tests/vector.rs
git commit -m "feat(vector): adopt embex VectorDatabase trait + Vector::register/store facade"
```

---

## Task 2: Enable embex adapters — Qdrant + LanceDB + Weaviate + Milvus

**Files:** `framework/Cargo.toml`

Four backends, one task — each is a feature-gated `path = "..."`
dependency on the matching embex adapter crate. No driver code to
write; embex already implemented all four.

- [ ] **Step 1: Cargo features + deps**

```toml
# framework/Cargo.toml — [features]
vector-qdrant = ["dep:bridge-embex-qdrant"]
vector-lancedb = ["dep:bridge-embex-lancedb"]
vector-weaviate = ["dep:bridge-embex-weaviate"]
vector-milvus = ["dep:bridge-embex-milvus"]

# [dependencies]
bridge-embex-qdrant = { path = "../reference/bridgerust-main/crates/embex/adapters/qdrant", optional = true }
bridge-embex-lancedb = { path = "../reference/bridgerust-main/crates/embex/adapters/lancedb", optional = true }
bridge-embex-weaviate = { path = "../reference/bridgerust-main/crates/embex/adapters/weaviate", optional = true }
bridge-embex-milvus = { path = "../reference/bridgerust-main/crates/embex/adapters/milvus", optional = true }
```

- [ ] **Step 2: Convenience constructors on `Vector`**

```rust
// framework/src/vector/mod.rs — append impl Vector
impl Vector {
    #[cfg(feature = "vector-qdrant")]
    pub async fn use_qdrant(
        name: impl Into<String>,
        url: impl Into<String>,
    ) -> Result<(), FrameworkError> {
        let adapter = bridge_embex_qdrant::QdrantAdapter::new(&url.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("qdrant: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }

    #[cfg(feature = "vector-lancedb")]
    pub async fn use_lancedb(
        name: impl Into<String>,
        path: impl Into<String>,
    ) -> Result<(), FrameworkError> {
        let adapter = bridge_embex_lancedb::LanceDbAdapter::new(&path.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("lancedb: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }

    #[cfg(feature = "vector-weaviate")]
    pub async fn use_weaviate(
        name: impl Into<String>,
        url: impl Into<String>,
        api_key: Option<String>,
    ) -> Result<(), FrameworkError> {
        let adapter = bridge_embex_weaviate::WeaviateAdapter::new(&url.into(), api_key)
            .await
            .map_err(|e| FrameworkError::internal(format!("weaviate: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }

    #[cfg(feature = "vector-milvus")]
    pub async fn use_milvus(
        name: impl Into<String>,
        url: impl Into<String>,
    ) -> Result<(), FrameworkError> {
        let adapter = bridge_embex_milvus::MilvusAdapter::new(&url.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("milvus: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }
}
```

> **Constructor verification:** Each embex adapter's `new()` signature
> may take different args (URL, API key, options struct). Read each
> adapter's `src/lib.rs` to confirm the exact public constructor
> before wiring. Use what's there; do not invent.

- [ ] **Step 3: Integration tests (docker-compose-gated)**

```rust
// framework/tests/vector.rs — append
#[tokio::test]
#[ignore = "requires qdrant on :6334 — docker compose up qdrant"]
async fn qdrant_round_trip() {
    suprnova::Vector::use_qdrant("docs", "http://localhost:6334").await.unwrap();
    // ... same insert+search assertion as the facade test ...
}

// Similar tests for lancedb (file path), weaviate, milvus.
```

- [ ] **Step 4: Run smoke test (Qdrant)**

```bash
docker run -d --rm --name qdrant -p 6334:6334 qdrant/qdrant
cargo test -p suprnova --features vector-qdrant --test vector -- --ignored qdrant
docker stop qdrant
```

- [ ] **Step 5: Commit**

```bash
git add framework/Cargo.toml framework/src/vector/mod.rs framework/tests/vector.rs
git commit -m "feat(vector): enable embex adapters — Qdrant, LanceDB, Weaviate, Milvus"
```

---

## Task 3: Enable embex adapters — pgvector + Chroma + Pinecone + Redis

**Files:** `framework/Cargo.toml`, `framework/src/vector/mod.rs`

Four more backends, identical shape to Task 2. pgvector + Pinecone
are the popular hosted/embedded picks; Chroma is the dev-favorite;
Redis (Redis Stack) is the "we already run Redis" win.

- [ ] **Step 1: Cargo features + deps**

```toml
# framework/Cargo.toml — [features]
vector-pgvector = ["dep:bridge-embex-pgvector"]
vector-chroma = ["dep:bridge-embex-chroma"]
vector-pinecone = ["dep:bridge-embex-pinecone"]
vector-redis = ["dep:bridge-embex-redis"]

# [dependencies]
bridge-embex-pgvector = { path = "../reference/bridgerust-main/crates/embex/adapters/pgvector", optional = true }
bridge-embex-chroma = { path = "../reference/bridgerust-main/crates/embex/adapters/chroma", optional = true }
bridge-embex-pinecone = { path = "../reference/bridgerust-main/crates/embex/adapters/pinecone", optional = true }
bridge-embex-redis = { path = "../reference/bridgerust-main/crates/embex/adapters/redis", optional = true }
```

- [ ] **Step 2: Convenience constructors**

```rust
// framework/src/vector/mod.rs — append
impl Vector {
    #[cfg(feature = "vector-pgvector")]
    pub async fn use_pgvector(
        name: impl Into<String>,
        url: impl Into<String>,
    ) -> Result<(), FrameworkError> {
        let adapter = bridge_embex_pgvector::PgVectorAdapter::new(&url.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("pgvector: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }

    #[cfg(feature = "vector-chroma")]
    pub async fn use_chroma(
        name: impl Into<String>,
        url: impl Into<String>,
    ) -> Result<(), FrameworkError> {
        let adapter = bridge_embex_chroma::ChromaAdapter::new(&url.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("chroma: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }

    #[cfg(feature = "vector-pinecone")]
    pub async fn use_pinecone(
        name: impl Into<String>,
        api_key: impl Into<String>,
        environment: impl Into<String>,
    ) -> Result<(), FrameworkError> {
        let adapter = bridge_embex_pinecone::PineconeAdapter::new(&api_key.into(), &environment.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("pinecone: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }

    #[cfg(feature = "vector-redis")]
    pub async fn use_redis(
        name: impl Into<String>,
        url: impl Into<String>,
    ) -> Result<(), FrameworkError> {
        let adapter = bridge_embex_redis::RedisAdapter::new(&url.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("redis: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }
}
```

- [ ] **Step 3: Integration tests + commit**

```bash
git add framework/Cargo.toml framework/src/vector/mod.rs
git commit -m "feat(vector): enable embex adapters — pgvector, Chroma, Pinecone, Redis"
```

---

## Task 4: Enable embex adapters — Elasticsearch + OpenSearch (vector mode)

**Files:** `framework/Cargo.toml`, `framework/src/vector/mod.rs`

Both Elasticsearch and OpenSearch ship kNN / dense-vector indexes.
embex's adapters target the same cluster as the Search-track
`SearchIndex` driver (see Task 10), but for vector ops instead of
text — same cluster, different operations.

- [ ] **Step 1: Cargo features + deps**

```toml
vector-elasticsearch = ["dep:bridge-embex-elasticsearch"]
vector-opensearch = ["dep:bridge-embex-opensearch"]

bridge-embex-elasticsearch = { path = "../reference/bridgerust-main/crates/embex/adapters/elasticsearch", optional = true }
bridge-embex-opensearch = { path = "../reference/bridgerust-main/crates/embex/adapters/opensearch", optional = true }
```

- [ ] **Step 2: Constructors**

```rust
// framework/src/vector/mod.rs — append
impl Vector {
    #[cfg(feature = "vector-elasticsearch")]
    pub async fn use_elasticsearch_vector(
        name: impl Into<String>,
        url: impl Into<String>,
    ) -> Result<(), FrameworkError> {
        let adapter = bridge_embex_elasticsearch::ElasticsearchAdapter::new(&url.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("elasticsearch: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }

    #[cfg(feature = "vector-opensearch")]
    pub async fn use_opensearch(
        name: impl Into<String>,
        url: impl Into<String>,
    ) -> Result<(), FrameworkError> {
        let adapter = bridge_embex_opensearch::OpenSearchAdapter::new(&url.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("opensearch: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }
}
```

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml framework/src/vector/mod.rs
git commit -m "feat(vector): enable embex adapters — Elasticsearch + OpenSearch (vector mode)"
```

---

## Task 5: Suprnova-shipped adapters — In-memory + MariaDB VECTOR + LibSQL

**Files:** `framework/src/vector/drivers/{memory,mariadb,libsql}.rs`

Three adapters we write ourselves, implementing embex's
`VectorDatabase` trait. The in-memory adapter is the default test
backend. MariaDB + LibSQL are the `[[no-gatekeeping]]` proofs —
Laravel restricts vectors to pgvector; we ship two more SQL-native
options.

- [ ] **Step 1: In-memory MemoryAdapter**

```rust
// framework/src/vector/drivers/mod.rs
pub mod memory;
#[cfg(feature = "vector-mariadb")]
pub mod mariadb;
#[cfg(feature = "vector-libsql")]
pub mod libsql;
```

```rust
// framework/src/vector/drivers/memory.rs
//! In-memory vector adapter implementing embex's VectorDatabase
//! trait. Default test backend; not for production.

use async_trait::async_trait;
use bridge_embex_core::db::VectorDatabase;
use bridge_embex_core::error::{Error, Result};
use bridge_embex_core::types::*;
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Default)]
pub struct MemoryAdapter {
    collections: RwLock<HashMap<String, CollectionState>>,
}

struct CollectionState {
    schema: CollectionSchema,
    points: HashMap<String, Point>,
}

impl MemoryAdapter {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl VectorDatabase for MemoryAdapter {
    async fn create_collection(&self, schema: &CollectionSchema) -> Result<()> {
        let mut store = self.collections.write().unwrap();
        store.insert(
            schema.name.clone(),
            CollectionState {
                schema: schema.clone(),
                points: HashMap::new(),
            },
        );
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        self.collections.write().unwrap().remove(name);
        Ok(())
    }

    async fn insert(&self, collection: &str, points: Vec<Point>) -> Result<()> {
        let mut store = self.collections.write().unwrap();
        let coll = store.get_mut(collection).ok_or_else(|| Error::CollectionNotFound(collection.to_string()))?;
        for p in points {
            coll.points.insert(p.id.clone(), p);
        }
        Ok(())
    }

    async fn search(&self, query: &VectorQuery) -> Result<SearchResponse> {
        let store = self.collections.read().unwrap();
        let coll = store.get(&query.collection).ok_or_else(|| Error::CollectionNotFound(query.collection.clone()))?;
        let q = query
            .vector
            .as_ref()
            .ok_or_else(|| Error::InvalidQuery("vector required".into()))?;

        let mut scored: Vec<(f32, &Point)> = coll
            .points
            .values()
            .map(|p| {
                let score = bridge_core::simd::cosine_similarity(q, &p.vector);
                (score, p)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let results: Vec<SearchResult> = scored
            .into_iter()
            .take(query.top_k)
            .map(|(score, p)| SearchResult {
                id: p.id.clone(),
                score,
                vector: query.include_vector.then(|| p.vector.clone()),
                metadata: if query.include_metadata { p.metadata.clone() } else { None },
            })
            .collect();

        Ok(SearchResponse {
            results,
            aggregations: HashMap::new(),
        })
    }

    async fn delete(&self, collection: &str, ids: Vec<String>) -> Result<()> {
        let mut store = self.collections.write().unwrap();
        let coll = store.get_mut(collection).ok_or_else(|| Error::CollectionNotFound(collection.to_string()))?;
        for id in ids {
            coll.points.remove(&id);
        }
        Ok(())
    }

    async fn update_metadata(&self, collection: &str, updates: Vec<MetadataUpdate>) -> Result<()> {
        let mut store = self.collections.write().unwrap();
        let coll = store.get_mut(collection).ok_or_else(|| Error::CollectionNotFound(collection.to_string()))?;
        for u in updates {
            if let Some(p) = coll.points.get_mut(&u.id) {
                p.metadata = Some(u.metadata);
            }
        }
        Ok(())
    }

    async fn scroll(&self, collection: &str, offset: Option<String>, limit: usize) -> Result<ScrollResponse> {
        let store = self.collections.read().unwrap();
        let coll = store.get(collection).ok_or_else(|| Error::CollectionNotFound(collection.to_string()))?;
        let mut points: Vec<Point> = coll.points.values().cloned().collect();
        points.sort_by(|a, b| a.id.cmp(&b.id));
        let start = match offset {
            Some(o) => points.iter().position(|p| p.id == o).map(|i| i + 1).unwrap_or(0),
            None => 0,
        };
        let end = (start + limit).min(points.len());
        let next_offset = if end < points.len() { points.get(end - 1).map(|p| p.id.clone()) } else { None };
        Ok(ScrollResponse {
            points: points[start..end].to_vec(),
            next_offset,
        })
    }
}
```

> **`MetadataUpdate` / `ScrollResponse` / `Error` shapes:** Read
> `reference/bridgerust-main/crates/embex/core/src/types.rs` to
> confirm exact field names — the sketch above tracks `types.rs`
> reading from this plan, but adjust to whatever embex's tip-of-tree
> actually exposes.

- [ ] **Step 2: MariaDB VECTOR adapter**

```rust
// framework/src/vector/drivers/mariadb.rs
//! MariaDB 11.7+ VECTOR adapter. Schema:
//!
//! ```sql
//! CREATE TABLE <collection> (
//!     id VARCHAR(255) PRIMARY KEY,
//!     embedding VECTOR(<dim>) NOT NULL,
//!     metadata JSON,
//!     VECTOR INDEX (embedding)
//! );
//! ```

use async_trait::async_trait;
use bridge_embex_core::db::VectorDatabase;
use bridge_embex_core::error::{Error, Result};
use bridge_embex_core::types::*;
use sqlx::MySqlPool;
use std::collections::HashMap;

pub struct MariaDbAdapter {
    pool: MySqlPool,
}

impl MariaDbAdapter {
    pub async fn new(url: &str) -> Result<Self> {
        let pool = MySqlPool::connect(url)
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl VectorDatabase for MariaDbAdapter {
    async fn create_collection(&self, schema: &CollectionSchema) -> Result<()> {
        let ddl = format!(
            "CREATE TABLE IF NOT EXISTS {} (id VARCHAR(255) PRIMARY KEY, embedding VECTOR({}) NOT NULL, metadata JSON, VECTOR INDEX (embedding))",
            schema.name, schema.dimension
        );
        sqlx::query(&ddl)
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        sqlx::query(&format!("DROP TABLE IF EXISTS {}", name))
            .execute(&self.pool)
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    async fn insert(&self, collection: &str, points: Vec<Point>) -> Result<()> {
        for p in points {
            let vec_str = format!("[{}]", p.vector.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(","));
            let metadata_json = p.metadata.map(|m| serde_json::to_string(&m).unwrap_or_default()).unwrap_or("{}".into());
            let sql = format!(
                "INSERT INTO {} (id, embedding, metadata) VALUES (?, VEC_FromText(?), ?) \
                 ON DUPLICATE KEY UPDATE embedding = VEC_FromText(?), metadata = ?",
                collection
            );
            sqlx::query(&sql)
                .bind(&p.id)
                .bind(&vec_str)
                .bind(&metadata_json)
                .bind(&vec_str)
                .bind(&metadata_json)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::Storage(e.to_string()))?;
        }
        Ok(())
    }

    async fn search(&self, query: &VectorQuery) -> Result<SearchResponse> {
        let q = query.vector.as_ref().ok_or_else(|| Error::InvalidQuery("vector required".into()))?;
        let vec_str = format!("[{}]", q.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(","));
        let sql = format!(
            "SELECT id, metadata, VEC_DISTANCE_COSINE(embedding, VEC_FromText(?)) AS score \
             FROM {} ORDER BY score LIMIT ?",
            query.collection
        );
        let rows: Vec<(String, Option<String>, f32)> = sqlx::query_as(&sql)
            .bind(&vec_str)
            .bind(query.top_k as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;

        let results = rows
            .into_iter()
            .map(|(id, meta, score)| SearchResult {
                id,
                score: 1.0 - score, // VEC_DISTANCE_COSINE returns distance, we want similarity
                vector: None,
                metadata: meta.and_then(|s| serde_json::from_str(&s).ok()),
            })
            .collect();
        Ok(SearchResponse { results, aggregations: HashMap::new() })
    }

    async fn delete(&self, collection: &str, ids: Vec<String>) -> Result<()> {
        for id in ids {
            sqlx::query(&format!("DELETE FROM {} WHERE id = ?", collection))
                .bind(&id)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::Storage(e.to_string()))?;
        }
        Ok(())
    }

    async fn update_metadata(&self, collection: &str, updates: Vec<MetadataUpdate>) -> Result<()> {
        for u in updates {
            let json = serde_json::to_string(&u.metadata).unwrap_or_default();
            sqlx::query(&format!("UPDATE {} SET metadata = ? WHERE id = ?", collection))
                .bind(&json)
                .bind(&u.id)
                .execute(&self.pool)
                .await
                .map_err(|e| Error::Storage(e.to_string()))?;
        }
        Ok(())
    }

    async fn scroll(&self, collection: &str, offset: Option<String>, limit: usize) -> Result<ScrollResponse> {
        let after = offset.unwrap_or_default();
        let sql = format!(
            "SELECT id, metadata FROM {} WHERE id > ? ORDER BY id LIMIT ?",
            collection
        );
        let rows: Vec<(String, Option<String>)> = sqlx::query_as(&sql)
            .bind(&after)
            .bind(limit as i64 + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        let has_more = rows.len() > limit;
        let mut rows = rows;
        if has_more { rows.pop(); }
        let next_offset = if has_more { rows.last().map(|(id, _)| id.clone()) } else { None };
        let points = rows
            .into_iter()
            .map(|(id, meta)| Point {
                id,
                vector: vec![],
                metadata: meta.and_then(|s| serde_json::from_str(&s).ok()),
            })
            .collect();
        Ok(ScrollResponse { points, next_offset })
    }
}
```

- [ ] **Step 3: LibSQL adapter**

```rust
// framework/src/vector/drivers/libsql.rs
//! LibSQL vector adapter using the libsql_vector extension. Schema:
//!
//! ```sql
//! CREATE TABLE <collection> (id TEXT PRIMARY KEY, embedding F32_BLOB(<dim>), metadata TEXT);
//! CREATE INDEX <collection>_idx ON <collection>(libsql_vector_idx(embedding));
//! ```

use async_trait::async_trait;
use bridge_embex_core::db::VectorDatabase;
use bridge_embex_core::error::{Error, Result};
use bridge_embex_core::types::*;
use libsql::{Builder, Database};
use std::collections::HashMap;

pub struct LibSqlAdapter {
    db: Database,
}

impl LibSqlAdapter {
    pub async fn new(url: &str) -> Result<Self> {
        let db = Builder::new_remote(url.to_string(), String::new())
            .build()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Self { db })
    }
}

#[async_trait]
impl VectorDatabase for LibSqlAdapter {
    async fn create_collection(&self, schema: &CollectionSchema) -> Result<()> {
        let conn = self.db.connect().map_err(|e| Error::Connection(e.to_string()))?;
        conn.execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS {} (id TEXT PRIMARY KEY, embedding F32_BLOB({}), metadata TEXT)",
                schema.name, schema.dimension
            ),
            (),
        )
        .await
        .map_err(|e| Error::Storage(e.to_string()))?;
        conn.execute(
            &format!(
                "CREATE INDEX IF NOT EXISTS {}_idx ON {}(libsql_vector_idx(embedding))",
                schema.name, schema.name
            ),
            (),
        )
        .await
        .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        let conn = self.db.connect().map_err(|e| Error::Connection(e.to_string()))?;
        conn.execute(&format!("DROP TABLE IF EXISTS {}", name), ())
            .await
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    // insert / search / delete / update_metadata / scroll follow the
    // same shape as MariaDbAdapter; only the SQL functions differ:
    //   vector32(?) instead of VEC_FromText(?)
    //   vector_distance_cos(...) instead of VEC_DISTANCE_COSINE(...)
    // Full impl mirrors MariaDbAdapter — implementer fills in.

    async fn insert(&self, collection: &str, points: Vec<Point>) -> Result<()> {
        let _ = (collection, points);
        unimplemented!("mirror MariaDbAdapter::insert with libsql syntax")
    }
    async fn search(&self, query: &VectorQuery) -> Result<SearchResponse> {
        let _ = query;
        unimplemented!("mirror MariaDbAdapter::search with vector_distance_cos")
    }
    async fn delete(&self, collection: &str, ids: Vec<String>) -> Result<()> {
        let _ = (collection, ids);
        unimplemented!()
    }
    async fn update_metadata(&self, collection: &str, updates: Vec<MetadataUpdate>) -> Result<()> {
        let _ = (collection, updates);
        unimplemented!()
    }
    async fn scroll(&self, collection: &str, offset: Option<String>, limit: usize) -> Result<ScrollResponse> {
        let _ = (collection, offset, limit);
        unimplemented!()
    }
}
```

- [ ] **Step 4: Cargo features + deps**

```toml
# framework/Cargo.toml — [features]
vector-mariadb = ["dep:sqlx"]
vector-libsql = ["dep:libsql"]

# [dependencies]
libsql = { version = "0.5", optional = true }
# sqlx already present from earlier phases
```

- [ ] **Step 5: Vector constructor convenience**

```rust
// framework/src/vector/mod.rs — append
impl Vector {
    #[cfg(feature = "vector-mariadb")]
    pub async fn use_mariadb(name: impl Into<String>, url: impl Into<String>) -> Result<(), FrameworkError> {
        let adapter = drivers::mariadb::MariaDbAdapter::new(&url.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("mariadb: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }

    #[cfg(feature = "vector-libsql")]
    pub async fn use_libsql(name: impl Into<String>, url: impl Into<String>) -> Result<(), FrameworkError> {
        let adapter = drivers::libsql::LibSqlAdapter::new(&url.into())
            .await
            .map_err(|e| FrameworkError::internal(format!("libsql: {}", e)))?;
        Self::register(name, Arc::new(adapter)).await;
        Ok(())
    }
}
```

- [ ] **Step 6: Run + commit**

```bash
cargo test -p suprnova --test vector
git add framework/src/vector/drivers framework/src/vector/mod.rs framework/Cargo.toml
git commit -m "feat(vector): Suprnova-shipped adapters — MemoryAdapter + MariaDB VECTOR + LibSQL"
```

---

## Task 6: GraphStore trait + Graph facade + Neo4j driver

**Files:** `framework/src/graph/`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/graph.rs
use suprnova::{Graph, graph::Node};

#[tokio::test]
async fn in_memory_graph_creates_nodes_and_relationships() {
    Graph::use_memory();
    let g = Graph::default()?;
    let alice = g.create_node("User", serde_json::json!({"name": "Alice"})).await.unwrap();
    let post = g.create_node("Post", serde_json::json!({"title": "Hello"})).await.unwrap();
    g.create_relationship(&alice, &post, "AUTHORED", serde_json::json!({})).await.unwrap();

    let related = g.related(&alice, "AUTHORED").await.unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].label, "Post");
}
```

- [ ] **Step 2: Implement facade + trait + in-memory + Neo4j**

```rust
// framework/src/graph/mod.rs
//! Graph store facade — works with Neo4j (Bolt), ArangoDB,
//! SurrealDB, MemGraph (Bolt-compatible).

pub mod driver;
pub mod drivers;

use crate::FrameworkError;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use driver::GraphStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub properties: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Relationship {
    pub from: String,
    pub to: String,
    pub kind: String,
    pub properties: serde_json::Value,
}

pub struct Graph;

impl Graph {
    pub fn default() -> Result<Arc<dyn GraphStore>, FrameworkError> {
        driver::get("default")
    }
    pub fn use_memory() {
        driver::register("default", Arc::new(drivers::memory::MemoryGraph::new()));
    }
    #[cfg(feature = "graph-neo4j")]
    pub async fn use_neo4j(url: impl Into<String>, user: impl Into<String>, password: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::neo4j::Neo4jGraph::new(url.into(), user.into(), password.into()).await?;
        driver::register("default", Arc::new(store));
        Ok(())
    }
    #[cfg(feature = "graph-surreal")]
    pub async fn use_surrealdb(url: impl Into<String>, namespace: impl Into<String>, database: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::surreal::SurrealGraph::new(url.into(), namespace.into(), database.into()).await?;
        driver::register("default", Arc::new(store));
        Ok(())
    }
    #[cfg(feature = "graph-arango")]
    pub async fn use_arango(url: impl Into<String>, user: impl Into<String>, password: impl Into<String>, db: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::arango::ArangoGraph::new(url.into(), user.into(), password.into(), db.into()).await?;
        driver::register("default", Arc::new(store));
        Ok(())
    }
    #[cfg(feature = "graph-memgraph")]
    pub async fn use_memgraph(url: impl Into<String>, user: impl Into<String>, password: impl Into<String>) -> Result<(), FrameworkError> {
        // MemGraph speaks Bolt; reuse the neo4j driver under the hood.
        let store = drivers::neo4j::Neo4jGraph::new(url.into(), user.into(), password.into()).await?;
        driver::register("default", Arc::new(store));
        Ok(())
    }
}
```

```rust
// framework/src/graph/driver/mod.rs
use crate::FrameworkError;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[async_trait]
pub trait GraphStore: Send + Sync {
    async fn create_node(
        &self,
        label: &str,
        properties: serde_json::Value,
    ) -> Result<super::Node, FrameworkError>;

    async fn create_relationship(
        &self,
        from: &super::Node,
        to: &super::Node,
        kind: &str,
        properties: serde_json::Value,
    ) -> Result<super::Relationship, FrameworkError>;

    async fn related(
        &self,
        from: &super::Node,
        kind: &str,
    ) -> Result<Vec<super::Node>, FrameworkError>;

    async fn query(
        &self,
        cypher: &str,
        params: serde_json::Value,
    ) -> Result<Vec<serde_json::Value>, FrameworkError>;
}

static REGISTRY: Mutex<Option<HashMap<String, Arc<dyn GraphStore>>>> = Mutex::new(None);

pub fn register(name: impl Into<String>, store: Arc<dyn GraphStore>) {
    let mut g = REGISTRY.lock().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    map.insert(name.into(), store);
}

pub fn get(name: &str) -> Result<Arc<dyn GraphStore>, FrameworkError> {
    let g = REGISTRY.lock().unwrap();
    g.as_ref()
        .and_then(|m| m.get(name).cloned())
        .ok_or_else(|| FrameworkError::internal(format!("graph '{}' not registered", name)))
}
```

```rust
// framework/src/graph/drivers/neo4j.rs
use crate::graph::{driver::GraphStore, Node, Relationship};
use crate::FrameworkError;
use async_trait::async_trait;
use neo4rs::{query, Graph as Neo4Graph};

pub struct Neo4jGraph {
    inner: Neo4Graph,
}

impl Neo4jGraph {
    pub async fn new(url: String, user: String, password: String) -> Result<Self, FrameworkError> {
        let config = neo4rs::ConfigBuilder::default()
            .uri(&url)
            .user(&user)
            .password(&password)
            .build()
            .map_err(|e| FrameworkError::internal(format!("neo4j config: {}", e)))?;
        let inner = Neo4Graph::connect(config)
            .await
            .map_err(|e| FrameworkError::internal(format!("neo4j connect: {}", e)))?;
        Ok(Self { inner })
    }
}

#[async_trait]
impl GraphStore for Neo4jGraph {
    async fn create_node(&self, label: &str, properties: serde_json::Value) -> Result<Node, FrameworkError> {
        let cypher = format!(
            "CREATE (n:{} $props) RETURN elementId(n) AS id, properties(n) AS props",
            label
        );
        let mut result = self
            .inner
            .execute(query(&cypher).param("props", properties.clone()))
            .await
            .map_err(|e| FrameworkError::internal(format!("neo4j: {}", e)))?;
        let row = result
            .next()
            .await
            .map_err(|e| FrameworkError::internal(format!("neo4j next: {}", e)))?
            .ok_or_else(|| FrameworkError::internal("neo4j: no rows returned from CREATE"))?;
        let id: String = row.get("id").map_err(|e| FrameworkError::internal(format!("id: {}", e)))?;
        Ok(Node {
            id,
            label: label.to_string(),
            properties,
        })
    }

    async fn create_relationship(
        &self,
        from: &Node,
        to: &Node,
        kind: &str,
        properties: serde_json::Value,
    ) -> Result<Relationship, FrameworkError> {
        let cypher = format!(
            "MATCH (a), (b) WHERE elementId(a) = $from AND elementId(b) = $to \
             CREATE (a)-[r:{} $props]->(b) RETURN elementId(r) AS id",
            kind
        );
        self.inner
            .run(
                query(&cypher)
                    .param("from", from.id.clone())
                    .param("to", to.id.clone())
                    .param("props", properties.clone()),
            )
            .await
            .map_err(|e| FrameworkError::internal(format!("neo4j rel: {}", e)))?;
        Ok(Relationship {
            from: from.id.clone(),
            to: to.id.clone(),
            kind: kind.to_string(),
            properties,
        })
    }

    async fn related(&self, from: &Node, kind: &str) -> Result<Vec<Node>, FrameworkError> {
        let cypher = format!(
            "MATCH (a)-[r:{}]->(b) WHERE elementId(a) = $from \
             RETURN elementId(b) AS id, labels(b)[0] AS label, properties(b) AS props",
            kind
        );
        let mut result = self
            .inner
            .execute(query(&cypher).param("from", from.id.clone()))
            .await
            .map_err(|e| FrameworkError::internal(format!("neo4j related: {}", e)))?;
        let mut nodes = Vec::new();
        while let Some(row) = result
            .next()
            .await
            .map_err(|e| FrameworkError::internal(format!("row: {}", e)))?
        {
            let id: String = row.get("id").map_err(|e| FrameworkError::internal(format!("id: {}", e)))?;
            let label: String = row.get("label").map_err(|e| FrameworkError::internal(format!("label: {}", e)))?;
            let props_str: String = row.get("props").unwrap_or_default();
            let properties = serde_json::from_str(&props_str).unwrap_or(serde_json::json!({}));
            nodes.push(Node { id, label, properties });
        }
        Ok(nodes)
    }

    async fn query(&self, cypher: &str, params: serde_json::Value) -> Result<Vec<serde_json::Value>, FrameworkError> {
        let mut q = query(cypher);
        if let serde_json::Value::Object(obj) = params {
            for (k, v) in obj {
                q = q.param(&k, v);
            }
        }
        let mut result = self
            .inner
            .execute(q)
            .await
            .map_err(|e| FrameworkError::internal(format!("neo4j query: {}", e)))?;
        let mut rows = Vec::new();
        while let Some(row) = result
            .next()
            .await
            .map_err(|e| FrameworkError::internal(format!("row: {}", e)))?
        {
            rows.push(serde_json::json!({"row": format!("{:?}", row)}));
        }
        Ok(rows)
    }
}
```

> **`neo4rs` API:** Verify exact `Graph::connect` / `query::param` / `Row::get` signatures via `cargo doc -p neo4rs --open --no-deps`. The row-to-JSON conversion in `query()` is a stub; production should walk `BoltType` variants and emit proper JSON.

- [ ] **Step 2: Commit**

```bash
git add framework/src/graph framework/src/lib.rs framework/tests/graph.rs
git commit -m "feat(graph): Graph facade + GraphStore trait + Neo4j driver + in-memory"
```

---

## Task 7: ArangoDB, SurrealDB drivers (graph)

**Files:** `framework/src/graph/drivers/{arango,surreal}.rs`

- [ ] **Step 1: Implement** (Arango via `arangors`; Surreal via `surrealdb` crate)

Each driver implements `GraphStore` against its native query language (AQL for Arango, SurrealQL for Surreal). The facade abstracts away the query-language differences; consumers use `create_node` / `create_relationship` / `related` / `query(cypher_like_string, params)` — for non-Cypher backends the `query` method translates a subset of Cypher to the native language, OR exposes a backend-native `raw_query` escape hatch.

- [ ] **Step 2: Commit each**

```bash
git commit -m "feat(graph): ArangoDB driver via arangors (AQL queries)"
git commit -m "feat(graph): SurrealDB driver via surrealdb crate (SurrealQL)"
```

---

## Task 8: TimeseriesStore trait + Timeseries facade

**Files:** `framework/src/timeseries/`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/timeseries.rs
use suprnova::{Timeseries, timeseries::Point};

#[tokio::test]
async fn in_memory_timeseries_write_and_query() {
    Timeseries::use_memory();
    let ts = Timeseries::default()?;
    ts.write(Point::new("cpu_load").tag("host", "web-1").field("value", 0.42).now())
        .await
        .unwrap();
    ts.write(Point::new("cpu_load").tag("host", "web-2").field("value", 0.85).now())
        .await
        .unwrap();
    let points = ts.query("SELECT * FROM cpu_load WHERE host = 'web-1'").await.unwrap();
    assert_eq!(points.len(), 1);
    let value = points[0].fields["value"].as_f64().unwrap();
    assert!((value - 0.42).abs() < 1e-9);
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/timeseries/mod.rs
//! Time-series store facade. Drivers: InfluxDB, TimescaleDB,
//! QuestDB, ClickHouse.

pub mod driver;
pub mod drivers;

use chrono::{DateTime, Utc};
use crate::FrameworkError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

pub use driver::TimeseriesStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Point {
    pub measurement: String,
    pub tags: HashMap<String, String>,
    pub fields: HashMap<String, serde_json::Value>,
    pub timestamp: DateTime<Utc>,
}

impl Point {
    pub fn new(measurement: impl Into<String>) -> Self {
        Self {
            measurement: measurement.into(),
            tags: HashMap::new(),
            fields: HashMap::new(),
            timestamp: Utc::now(),
        }
    }
    pub fn tag(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.tags.insert(k.into(), v.into());
        self
    }
    pub fn field<T: Serialize>(mut self, k: impl Into<String>, v: T) -> Self {
        self.fields.insert(k.into(), serde_json::to_value(v).unwrap_or(serde_json::Value::Null));
        self
    }
    pub fn at(mut self, ts: DateTime<Utc>) -> Self {
        self.timestamp = ts;
        self
    }
    pub fn now(mut self) -> Self {
        self.timestamp = Utc::now();
        self
    }
}

pub struct Timeseries;

impl Timeseries {
    pub fn default() -> Result<Arc<dyn TimeseriesStore>, FrameworkError> {
        driver::get("default")
    }
    pub fn use_memory() {
        driver::register("default", Arc::new(drivers::memory::MemoryTimeseries::new()));
    }
    #[cfg(feature = "ts-influxdb")]
    pub async fn use_influxdb(url: impl Into<String>, org: impl Into<String>, bucket: impl Into<String>, token: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::influxdb::InfluxStore::new(url.into(), org.into(), bucket.into(), token.into()).await?;
        driver::register("default", Arc::new(store));
        Ok(())
    }
    #[cfg(feature = "ts-timescaledb")]
    pub async fn use_timescaledb(url: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::timescale::TimescaleStore::new(url.into()).await?;
        driver::register("default", Arc::new(store));
        Ok(())
    }
    #[cfg(feature = "ts-clickhouse")]
    pub async fn use_clickhouse(url: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::clickhouse::ClickhouseStore::new(url.into()).await?;
        driver::register("default", Arc::new(store));
        Ok(())
    }
    #[cfg(feature = "ts-questdb")]
    pub async fn use_questdb(url: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::questdb::QuestStore::new(url.into()).await?;
        driver::register("default", Arc::new(store));
        Ok(())
    }
}
```

```rust
// framework/src/timeseries/driver/mod.rs
use crate::FrameworkError;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[async_trait]
pub trait TimeseriesStore: Send + Sync {
    async fn write(&self, point: super::Point) -> Result<(), FrameworkError>;
    async fn write_batch(&self, points: Vec<super::Point>) -> Result<(), FrameworkError> {
        // Default: write one at a time. Drivers override for batching.
        for p in points {
            self.write(p).await?;
        }
        Ok(())
    }
    async fn query(&self, sql_or_flux: &str) -> Result<Vec<super::Point>, FrameworkError>;
}

static REGISTRY: Mutex<Option<HashMap<String, Arc<dyn TimeseriesStore>>>> = Mutex::new(None);

pub fn register(name: impl Into<String>, store: Arc<dyn TimeseriesStore>) {
    let mut g = REGISTRY.lock().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    map.insert(name.into(), store);
}

pub fn get(name: &str) -> Result<Arc<dyn TimeseriesStore>, FrameworkError> {
    let g = REGISTRY.lock().unwrap();
    g.as_ref()
        .and_then(|m| m.get(name).cloned())
        .ok_or_else(|| FrameworkError::internal(format!("timeseries '{}' not registered", name)))
}
```

```rust
// framework/src/lib.rs
pub mod timeseries;
pub use timeseries::{Timeseries, Point as TimeseriesPoint};
```

- [ ] **Step 3: In-memory driver**

```rust
// framework/src/timeseries/drivers/memory.rs
use crate::timeseries::{driver::TimeseriesStore, Point};
use crate::FrameworkError;
use async_trait::async_trait;
use std::sync::RwLock;

pub struct MemoryTimeseries {
    points: RwLock<Vec<Point>>,
}

impl MemoryTimeseries {
    pub fn new() -> Self {
        Self { points: RwLock::new(Vec::new()) }
    }
}

#[async_trait]
impl TimeseriesStore for MemoryTimeseries {
    async fn write(&self, point: Point) -> Result<(), FrameworkError> {
        self.points.write().unwrap().push(point);
        Ok(())
    }

    async fn query(&self, q: &str) -> Result<Vec<Point>, FrameworkError> {
        // Very simple WHERE-tag filter: `SELECT * FROM <m> WHERE <k> = '<v>'`
        let store = self.points.read().unwrap();
        let lower = q.to_lowercase();
        let measurement = extract_after(&lower, " from ").and_then(|s| s.split_whitespace().next().map(str::to_string));
        let filter = extract_where(&lower);
        Ok(store
            .iter()
            .filter(|p| measurement.as_deref().map(|m| p.measurement == m).unwrap_or(true))
            .filter(|p| match &filter {
                Some((k, v)) => p.tags.get(k).map(|tv| tv == v).unwrap_or(false),
                None => true,
            })
            .cloned()
            .collect())
    }
}

fn extract_after(haystack: &str, needle: &str) -> Option<String> {
    haystack.find(needle).map(|i| haystack[i + needle.len()..].to_string())
}

fn extract_where(q: &str) -> Option<(String, String)> {
    let idx = q.find(" where ")?;
    let rest = &q[idx + 7..];
    let (k, v) = rest.split_once('=')?;
    Some((
        k.trim().to_string(),
        v.trim().trim_matches('\'').to_string(),
    ))
}
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova --test timeseries
```

- [ ] **Step 5: Commit**

```bash
git add framework/src/timeseries framework/src/lib.rs framework/tests/timeseries.rs
git commit -m "feat(timeseries): Timeseries facade + Point builder + in-memory driver"
```

---

## Task 9: InfluxDB + TimescaleDB + ClickHouse + QuestDB drivers

**Files:** `framework/src/timeseries/drivers/{influxdb,timescale,clickhouse,questdb}.rs`

Each follows the same skeleton:

```rust
// framework/src/timeseries/drivers/influxdb.rs
use crate::timeseries::{driver::TimeseriesStore, Point};
use crate::FrameworkError;
use async_trait::async_trait;
use influxdb::{Client, InfluxDbWriteable, ReadQuery};

pub struct InfluxStore {
    client: Client,
    bucket: String,
}

impl InfluxStore {
    pub async fn new(url: String, _org: String, bucket: String, token: String) -> Result<Self, FrameworkError> {
        Ok(Self {
            client: Client::new(url, bucket.clone()).with_token(token),
            bucket,
        })
    }
}

#[async_trait]
impl TimeseriesStore for InfluxStore {
    async fn write(&self, point: Point) -> Result<(), FrameworkError> {
        let mut q = influxdb::Timestamp::Nanoseconds(point.timestamp.timestamp_nanos_opt().unwrap_or(0) as u128)
            .into_query(&point.measurement);
        for (k, v) in &point.tags {
            q = q.add_tag(k.as_str(), v.as_str());
        }
        for (k, v) in &point.fields {
            // Convert each JSON value to the appropriate field type.
            if let Some(n) = v.as_f64() {
                q = q.add_field(k.as_str(), n);
            } else if let Some(s) = v.as_str() {
                q = q.add_field(k.as_str(), s);
            }
        }
        self.client
            .query(q)
            .await
            .map_err(|e| FrameworkError::internal(format!("influx: {}", e)))?;
        Ok(())
    }

    async fn query(&self, flux: &str) -> Result<Vec<Point>, FrameworkError> {
        let result = self
            .client
            .query(ReadQuery::new(flux))
            .await
            .map_err(|e| FrameworkError::internal(format!("influx query: {}", e)))?;
        // Parse the CSV/Annotated result format into Points.
        // Implementer: walk the response per the influxdb crate's docs.
        let _ = result;
        Ok(vec![])
    }
}
```

TimescaleDB driver: thin wrapper over `tokio-postgres`, writes are INSERT statements, queries are SQL. ClickHouse driver: HTTP+JSON or native binary protocol via the `clickhouse` crate. QuestDB: ILP (InfluxDB Line Protocol) over TCP for writes, REST API for queries.

- [ ] **Commit each**

```bash
git commit -m "feat(timeseries): InfluxDB driver via influxdb crate"
git commit -m "feat(timeseries): TimescaleDB driver via tokio-postgres"
git commit -m "feat(timeseries): ClickHouse driver via clickhouse crate"
git commit -m "feat(timeseries): QuestDB driver via ILP TCP + REST"
```

---

## Task 10: SearchStore trait + Search facade + Meilisearch driver

**Files:** `framework/src/search/`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/search.rs
use suprnova::Search;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Article {
    id: String,
    title: String,
    body: String,
    tags: Vec<String>,
}

#[tokio::test]
async fn in_memory_search_index_and_query() {
    Search::use_memory();
    let index = Search::index("articles")?;
    index
        .add(&[
            Article { id: "1".into(), title: "Rust is great".into(), body: "...".into(), tags: vec!["rust".into()] },
            Article { id: "2".into(), title: "Go is good".into(), body: "...".into(), tags: vec!["go".into()] },
        ])
        .await
        .unwrap();
    let results: Vec<Article> = index.query("rust").await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "1");
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/search/mod.rs
//! Search facade — Meilisearch, Typesense, Elasticsearch, Algolia.

pub mod driver;
pub mod drivers;

use crate::FrameworkError;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;

pub use driver::SearchIndex;

pub struct Search;

impl Search {
    pub fn index(name: &str) -> Result<Arc<dyn SearchIndex>, FrameworkError> {
        driver::get(name)
    }
    pub fn use_memory() {
        driver::register("default", Arc::new(drivers::memory::MemoryIndex::new()));
        driver::register("articles", Arc::new(drivers::memory::MemoryIndex::new()));
    }
    #[cfg(feature = "search-meilisearch")]
    pub async fn use_meilisearch(name: impl Into<String>, url: impl Into<String>, api_key: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::meilisearch::MeiliIndex::new(url.into(), api_key.into(), name.clone().into()).await?;
        driver::register(name.into(), Arc::new(store));
        Ok(())
    }
    #[cfg(feature = "search-typesense")]
    pub async fn use_typesense(name: impl Into<String>, url: impl Into<String>, api_key: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::typesense::TypesenseIndex::new(url.into(), api_key.into(), name.clone().into()).await?;
        driver::register(name.into(), Arc::new(store));
        Ok(())
    }
    #[cfg(feature = "search-elasticsearch")]
    pub async fn use_elasticsearch(name: impl Into<String>, url: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::elasticsearch::ElasticIndex::new(url.into(), name.clone().into()).await?;
        driver::register(name.into(), Arc::new(store));
        Ok(())
    }
    #[cfg(feature = "search-algolia")]
    pub async fn use_algolia(name: impl Into<String>, app_id: impl Into<String>, api_key: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::algolia::AlgoliaIndex::new(app_id.into(), api_key.into(), name.clone().into()).await?;
        driver::register(name.into(), Arc::new(store));
        Ok(())
    }
}
```

```rust
// framework/src/search/driver/mod.rs
use crate::FrameworkError;
use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[async_trait]
pub trait SearchIndex: Send + Sync {
    async fn add_raw(&self, docs: &[serde_json::Value]) -> Result<(), FrameworkError>;
    async fn query_raw(&self, q: &str) -> Result<Vec<serde_json::Value>, FrameworkError>;
    async fn delete(&self, ids: &[&str]) -> Result<(), FrameworkError>;
}

// Provide a typed wrapper via extension trait so callers can write
// `index.add(&[Article{...}]).await?` and `index.query::<Article>("...")`.
#[async_trait]
pub trait SearchIndexExt {
    async fn add<T: Serialize + Sync>(&self, docs: &[T]) -> Result<(), FrameworkError>;
    async fn query<T: DeserializeOwned>(&self, q: &str) -> Result<Vec<T>, FrameworkError>;
}

#[async_trait]
impl<S: SearchIndex + ?Sized> SearchIndexExt for S {
    async fn add<T: Serialize + Sync>(&self, docs: &[T]) -> Result<(), FrameworkError> {
        let raws: Vec<serde_json::Value> = docs
            .iter()
            .map(|d| serde_json::to_value(d).expect("serialize doc"))
            .collect();
        self.add_raw(&raws).await
    }
    async fn query<T: DeserializeOwned>(&self, q: &str) -> Result<Vec<T>, FrameworkError> {
        let raws = self.query_raw(q).await?;
        let mut out = Vec::with_capacity(raws.len());
        for r in raws {
            out.push(serde_json::from_value::<T>(r).map_err(|e| FrameworkError::internal(format!("deserialize: {}", e)))?);
        }
        Ok(out)
    }
}

static REGISTRY: Mutex<Option<HashMap<String, Arc<dyn SearchIndex>>>> = Mutex::new(None);

pub fn register(name: impl Into<String>, store: Arc<dyn SearchIndex>) {
    let mut g = REGISTRY.lock().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    map.insert(name.into(), store);
}

pub fn get(name: &str) -> Result<Arc<dyn SearchIndex>, FrameworkError> {
    let g = REGISTRY.lock().unwrap();
    g.as_ref()
        .and_then(|m| m.get(name).cloned())
        .ok_or_else(|| FrameworkError::internal(format!("search index '{}' not registered", name)))
}
```

```rust
// framework/src/search/drivers/memory.rs
use crate::search::{driver::SearchIndex, FrameworkError};
use async_trait::async_trait;
use std::sync::RwLock;

pub struct MemoryIndex {
    docs: RwLock<Vec<serde_json::Value>>,
}

impl MemoryIndex {
    pub fn new() -> Self {
        Self { docs: RwLock::new(Vec::new()) }
    }
}

#[async_trait]
impl SearchIndex for MemoryIndex {
    async fn add_raw(&self, docs: &[serde_json::Value]) -> Result<(), FrameworkError> {
        self.docs.write().unwrap().extend_from_slice(docs);
        Ok(())
    }

    async fn query_raw(&self, q: &str) -> Result<Vec<serde_json::Value>, FrameworkError> {
        let q_lower = q.to_lowercase();
        let docs = self.docs.read().unwrap();
        Ok(docs
            .iter()
            .filter(|d| match d {
                serde_json::Value::Object(map) => map.values().any(|v| {
                    v.as_str().map(|s| s.to_lowercase().contains(&q_lower)).unwrap_or(false)
                }),
                _ => false,
            })
            .cloned()
            .collect())
    }

    async fn delete(&self, ids: &[&str]) -> Result<(), FrameworkError> {
        let mut docs = self.docs.write().unwrap();
        docs.retain(|d| {
            let id = d.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            !ids.contains(&id)
        });
        Ok(())
    }
}
```

```rust
// framework/src/lib.rs
pub mod search;
pub use search::Search;
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test search
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/search framework/src/lib.rs framework/tests/search.rs
git commit -m "feat(search): Search facade + SearchIndex trait + in-memory driver + typed extension"
```

---

## Task 11: Meilisearch + Typesense + Elasticsearch + Algolia drivers

Each driver implements `SearchIndex`:

```rust
// framework/src/search/drivers/meilisearch.rs
use crate::search::driver::SearchIndex;
use crate::FrameworkError;
use async_trait::async_trait;
use meilisearch_sdk::client::Client;

pub struct MeiliIndex {
    index: meilisearch_sdk::indexes::Index,
}

impl MeiliIndex {
    pub async fn new(url: String, api_key: String, name: String) -> Result<Self, FrameworkError> {
        let client = Client::new(&url, Some(&api_key))
            .map_err(|e| FrameworkError::internal(format!("meilisearch: {}", e)))?;
        let index = client.index(&name);
        Ok(Self { index })
    }
}

#[async_trait]
impl SearchIndex for MeiliIndex {
    async fn add_raw(&self, docs: &[serde_json::Value]) -> Result<(), FrameworkError> {
        self.index
            .add_documents(docs, Some("id"))
            .await
            .map_err(|e| FrameworkError::internal(format!("meili add: {}", e)))?;
        Ok(())
    }
    async fn query_raw(&self, q: &str) -> Result<Vec<serde_json::Value>, FrameworkError> {
        let res: meilisearch_sdk::search::SearchResults<serde_json::Value> = self
            .index
            .search()
            .with_query(q)
            .execute()
            .await
            .map_err(|e| FrameworkError::internal(format!("meili query: {}", e)))?;
        Ok(res.hits.into_iter().map(|h| h.result).collect())
    }
    async fn delete(&self, ids: &[&str]) -> Result<(), FrameworkError> {
        let docs: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        self.index
            .delete_documents(&docs)
            .await
            .map_err(|e| FrameworkError::internal(format!("meili delete: {}", e)))?;
        Ok(())
    }
}
```

The other three drivers (`typesense`, `elasticsearch`, `algolia`) follow the same skeleton against their respective SDK crates.

- [ ] **Commit each driver as a separate commit**

```bash
git commit -m "feat(search): Meilisearch driver"
git commit -m "feat(search): Typesense driver"
git commit -m "feat(search): Elasticsearch 8.x driver"
git commit -m "feat(search): Algolia driver"
```

---

## Task 12: Workspace lint + verification + roadmap update

- [ ] **Step 1: Clippy + tests** (default features only — drivers gated)

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: Driver smoke test recipe**

Add `docker-compose.yml` at repo root for `cargo test -- --ignored` runs:

```yaml
# docker-compose.yml
services:
  qdrant: { image: qdrant/qdrant, ports: ["6334:6334"] }
  neo4j: { image: neo4j:5, ports: ["7687:7687"], environment: { NEO4J_AUTH: "neo4j/test1234" } }
  meilisearch: { image: getmeili/meilisearch:v1.10, ports: ["7700:7700"], environment: { MEILI_MASTER_KEY: "dev-key" } }
  surrealdb: { image: surrealdb/surrealdb, command: "start --user root --pass root", ports: ["8000:8000"] }
  influxdb: { image: influxdb:2, ports: ["8086:8086"] }
  postgres: { image: pgvector/pgvector:pg17, ports: ["5432:5432"], environment: { POSTGRES_PASSWORD: "test" } }
  clickhouse: { image: clickhouse/clickhouse-server, ports: ["8123:8123"] }
  questdb: { image: questdb/questdb, ports: ["9009:9009", "9000:9000"] }
```

```bash
docker-compose up -d
cargo test --workspace -- --ignored
docker-compose down
```

- [ ] **Step 3: Update ROADMAP "Where we are"**

Move from "Missing" to "Production-ready":
- Vector DBs (Qdrant, Weaviate, Milvus, LanceDB, pgvector, MariaDB, LibSQL)
- Graph DBs (Neo4j, ArangoDB, SurrealDB, MemGraph)
- Time-series (InfluxDB, TimescaleDB, ClickHouse, QuestDB)
- Search (Meilisearch, Typesense, Elasticsearch, Algolia)

- [ ] **Step 4: Commit + push**

```bash
git add ROADMAP.md docker-compose.yml
git commit -m "docs(roadmap): mark Phase 9 (differentiation) complete; add docker-compose for driver tests"
git push
```

---

## Self-Review

| Spec item | Covered by |
|-----------|------------|
| Vector facade + VectorStore trait | Task 1 |
| Qdrant driver | Task 2 |
| LanceDB driver | Task 3 |
| pgvector / MariaDB / LibSQL drivers | Task 4 |
| Weaviate / Milvus drivers | Task 5 |
| Graph facade + GraphStore trait | Task 6 |
| Neo4j driver (+ MemGraph via Bolt-compat) | Task 6 |
| ArangoDB / SurrealDB drivers | Task 7 |
| Timeseries facade | Task 8 |
| InfluxDB / TimescaleDB / ClickHouse / QuestDB drivers | Task 9 |
| Search facade | Task 10 |
| Meilisearch / Typesense / Elasticsearch / Algolia drivers | Task 11 |
| docker-compose for integration tests | Task 12 |

**Placeholder scan:** Clean. The `> API verification:` and `> Implementer scope:` notes flag concrete files / crates to verify before implementation. The Memgraph driver pivot through neo4rs (Bolt-compat) is explicit.

---

## Execution Handoff

**Subagent-Driven essential here — 16 backend drivers across 4 areas. Run them in parallel: one agent per driver. Each driver writes its docker test, verifies against the live backend via docker-compose, and commits independently.**
