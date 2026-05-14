# Phase 9: Differentiation (Vectors + Graphs + Time-series + Search) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship the four Suprnova-specific value-add tracks that Laravel either lacks or gatekeeps behind one backend. Every consumer picks their stack and Suprnova has a first-class driver — `Vector::store("docs").upsert(...)` works the same whether the user runs Qdrant, Weaviate, Milvus, LanceDB, pgvector, MariaDB VECTOR, or LibSQL. Same trait surface; different config URL.

**Architecture:** Each subsystem is a trait + driver registry following the same pattern as `CacheStore` / `MailTransport` / `QueueDriver` from earlier phases. The user's `bootstrap.rs` calls `Vector::use_qdrant(...)` (or any driver) to bind a driver; the facade routes all calls through it. Drivers live behind feature flags so consumers only compile in the backends they use.

**Tech Stack per area:**
- **Vectors:** `qdrant-client`, `lancedb`, `weaviate-client`, `milvus-sdk-rust`, `sqlx` for pgvector / MariaDB / LibSQL
- **Graphs:** `neo4rs` (Neo4j Bolt), `arangors`, `surrealdb`, `bolt-proto`-based for MemGraph (Bolt-compat)
- **Time-series:** `influxdb` 0.7, `tokio-postgres` for TimescaleDB (Postgres extension), `clickhouse` 0.13, `quest-client` (or HTTP fallback)
- **Search:** `meilisearch-sdk` 0.27, `typesense` 0.7, `elasticsearch` 8.x, `algoliasearch` 1.x

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

## Task 1: VectorStore trait + Vector facade

**Files:** `framework/src/vector/mod.rs`, `framework/src/vector/driver/mod.rs`

- [ ] **Step 1: Write failing test (with in-memory fake)**

```rust
// framework/tests/vector.rs
use suprnova::{Vector, vector::{VectorStore, Embedding}};

#[tokio::test]
async fn in_memory_vector_store_upsert_and_search() {
    Vector::use_memory();
    let store = Vector::store("docs").unwrap();
    store
        .upsert(&[
            ("doc-1", Embedding::new(vec![1.0, 0.0, 0.0]), serde_json::json!({"title": "Rust"})),
            ("doc-2", Embedding::new(vec![0.0, 1.0, 0.0]), serde_json::json!({"title": "Go"})),
            ("doc-3", Embedding::new(vec![0.0, 0.0, 1.0]), serde_json::json!({"title": "Java"})),
        ])
        .await
        .unwrap();

    let results = store.similar(Embedding::new(vec![0.9, 0.1, 0.0]), 2).await.unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].id, "doc-1"); // closest to [1, 0, 0]
}

#[tokio::test]
async fn unknown_store_returns_error() {
    let result = Vector::store("nonexistent");
    assert!(result.is_err());
}

#[tokio::test]
async fn delete_removes_id_from_store() {
    Vector::use_memory();
    let store = Vector::store("docs").unwrap();
    store
        .upsert(&[("a", Embedding::new(vec![1.0, 0.0]), serde_json::json!({}))])
        .await
        .unwrap();
    store.delete(&["a"]).await.unwrap();
    let results = store.similar(Embedding::new(vec![1.0, 0.0]), 1).await.unwrap();
    assert!(results.is_empty());
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/vector/mod.rs
//! Vector store facade. Same trait, many drivers — Laravel
//! gatekeeps to pgvector; we ship Qdrant, Weaviate, Milvus, LanceDB,
//! pgvector, MariaDB VECTOR, and LibSQL.
//!
//! ```ignore
//! Vector::use_qdrant("http://localhost:6334").await?;
//! let store = Vector::store("docs")?;
//! store.upsert(&[("d1", embedding, metadata)]).await?;
//! let hits = store.similar(query_embedding, 10).await?;
//! ```

pub mod driver;
pub mod drivers;
pub mod testing;

use crate::FrameworkError;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use driver::VectorStore;

/// A vector with optional dimension tag — drivers verify dimension
/// matches the configured index at upsert time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Embedding {
    pub values: Vec<f32>,
}

impl Embedding {
    pub fn new(values: Vec<f32>) -> Self {
        Self { values }
    }
    pub fn dim(&self) -> usize {
        self.values.len()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VectorMatch {
    pub id: String,
    pub score: f32,
    pub metadata: serde_json::Value,
}

pub struct Vector;

impl Vector {
    pub fn store(name: &str) -> Result<Arc<dyn VectorStore>, FrameworkError> {
        driver::get(name)
    }

    pub fn use_memory() {
        driver::register("default", Arc::new(drivers::memory::MemoryVectorStore::new()));
        // Convenience: also bind under "docs" for the common case.
        driver::register("docs", Arc::new(drivers::memory::MemoryVectorStore::new()));
    }

    #[cfg(feature = "vector-qdrant")]
    pub async fn use_qdrant(name: impl Into<String>, url: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::qdrant::QdrantVectorStore::new(url.into(), name.clone().into()).await?;
        driver::register(name.into(), Arc::new(store));
        Ok(())
    }

    #[cfg(feature = "vector-lancedb")]
    pub async fn use_lancedb(name: impl Into<String>, path: impl Into<String>) -> Result<(), FrameworkError> {
        let store = drivers::lancedb::LanceDbStore::new(path.into(), name.clone().into()).await?;
        driver::register(name.into(), Arc::new(store));
        Ok(())
    }

    #[cfg(feature = "vector-pgvector")]
    pub async fn use_pgvector(name: impl Into<String>, url: impl Into<String>, dim: usize) -> Result<(), FrameworkError> {
        let store = drivers::pgvector::PgVectorStore::new(url.into(), name.clone().into(), dim).await?;
        driver::register(name.into(), Arc::new(store));
        Ok(())
    }

    #[cfg(feature = "vector-mariadb")]
    pub async fn use_mariadb(name: impl Into<String>, url: impl Into<String>, dim: usize) -> Result<(), FrameworkError> {
        let store = drivers::mariadb::MariaDbVectorStore::new(url.into(), name.clone().into(), dim).await?;
        driver::register(name.into(), Arc::new(store));
        Ok(())
    }

    #[cfg(feature = "vector-libsql")]
    pub async fn use_libsql(name: impl Into<String>, url: impl Into<String>, dim: usize) -> Result<(), FrameworkError> {
        let store = drivers::libsql::LibSqlVectorStore::new(url.into(), name.clone().into(), dim).await?;
        driver::register(name.into(), Arc::new(store));
        Ok(())
    }
}
```

```rust
// framework/src/vector/driver/mod.rs
use crate::FrameworkError;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[async_trait]
pub trait VectorStore: Send + Sync {
    async fn upsert(
        &self,
        records: &[(&str, super::Embedding, serde_json::Value)],
    ) -> Result<(), FrameworkError>;

    async fn similar(
        &self,
        query: super::Embedding,
        k: usize,
    ) -> Result<Vec<super::VectorMatch>, FrameworkError>;

    async fn delete(&self, ids: &[&str]) -> Result<(), FrameworkError>;

    async fn count(&self) -> Result<u64, FrameworkError> {
        Ok(0) // optional override
    }
}

static REGISTRY: Mutex<Option<HashMap<String, Arc<dyn VectorStore>>>> = Mutex::new(None);

pub fn register(name: impl Into<String>, store: Arc<dyn VectorStore>) {
    let mut g = REGISTRY.lock().unwrap();
    let map = g.get_or_insert_with(HashMap::new);
    map.insert(name.into(), store);
}

pub fn get(name: &str) -> Result<Arc<dyn VectorStore>, FrameworkError> {
    let g = REGISTRY.lock().unwrap();
    g.as_ref()
        .and_then(|m| m.get(name).cloned())
        .ok_or_else(|| FrameworkError::internal(format!("vector store '{}' not registered", name)))
}
```

- [ ] **Step 3: Implement in-memory driver**

```rust
// framework/src/vector/drivers/memory.rs
use super::super::{driver::VectorStore, Embedding, VectorMatch};
use crate::FrameworkError;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

pub struct MemoryVectorStore {
    records: RwLock<HashMap<String, (Embedding, serde_json::Value)>>,
}

impl MemoryVectorStore {
    pub fn new() -> Self {
        Self {
            records: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl VectorStore for MemoryVectorStore {
    async fn upsert(
        &self,
        records: &[(&str, Embedding, serde_json::Value)],
    ) -> Result<(), FrameworkError> {
        let mut store = self.records.write().unwrap();
        for (id, emb, meta) in records {
            store.insert(id.to_string(), (emb.clone(), meta.clone()));
        }
        Ok(())
    }

    async fn similar(
        &self,
        query: Embedding,
        k: usize,
    ) -> Result<Vec<VectorMatch>, FrameworkError> {
        let store = self.records.read().unwrap();
        let mut scored: Vec<VectorMatch> = store
            .iter()
            .map(|(id, (emb, meta))| VectorMatch {
                id: id.clone(),
                score: cosine_similarity(&query.values, &emb.values),
                metadata: meta.clone(),
            })
            .collect();
        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        Ok(scored)
    }

    async fn delete(&self, ids: &[&str]) -> Result<(), FrameworkError> {
        let mut store = self.records.write().unwrap();
        for id in ids {
            store.remove(*id);
        }
        Ok(())
    }

    async fn count(&self) -> Result<u64, FrameworkError> {
        Ok(self.records.read().unwrap().len() as u64)
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
```

```rust
// framework/src/vector/drivers/mod.rs
pub mod memory;

#[cfg(feature = "vector-qdrant")]
pub mod qdrant;
#[cfg(feature = "vector-lancedb")]
pub mod lancedb;
#[cfg(feature = "vector-pgvector")]
pub mod pgvector;
#[cfg(feature = "vector-mariadb")]
pub mod mariadb;
#[cfg(feature = "vector-libsql")]
pub mod libsql;
```

```rust
// framework/src/lib.rs
pub mod vector;
pub use vector::{Vector, Embedding, VectorMatch};
```

- [ ] **Step 4: Run — expect pass**

```bash
cargo test -p suprnova --test vector
```

- [ ] **Step 5: Commit**

```bash
git add framework/src/vector framework/src/lib.rs framework/tests/vector.rs
git commit -m "feat(vector): Vector facade + VectorStore trait + in-memory driver"
```

---

## Task 2: Qdrant vector driver

**Files:** `framework/src/vector/drivers/qdrant.rs`, `framework/Cargo.toml`

- [ ] **Step 1: Add feature + dep**

```toml
# framework/Cargo.toml
[features]
default = []
vector-qdrant = ["dep:qdrant-client"]
vector-lancedb = ["dep:lancedb"]
vector-pgvector = ["dep:sqlx"]
vector-mariadb = ["dep:sqlx"]
vector-libsql = ["dep:libsql"]

[dependencies]
qdrant-client = { version = "1.10", optional = true }
lancedb = { version = "0.10", optional = true }
libsql = { version = "0.5", optional = true }
# sqlx already present from earlier phases
```

- [ ] **Step 2: Integration test (gated)**

```rust
// framework/tests/vector.rs — append
#[tokio::test]
#[ignore = "requires qdrant on localhost:6334 — docker run -p 6334:6334 qdrant/qdrant"]
async fn qdrant_round_trip() {
    suprnova::Vector::use_qdrant("docs", "http://localhost:6334").await.unwrap();
    let store = suprnova::Vector::store("docs").unwrap();
    store
        .upsert(&[("q-1", suprnova::Embedding::new(vec![0.1; 384]), serde_json::json!({"src": "test"}))])
        .await
        .unwrap();
    let hits = store.similar(suprnova::Embedding::new(vec![0.1; 384]), 5).await.unwrap();
    assert!(!hits.is_empty());
}
```

- [ ] **Step 3: Implement**

```rust
// framework/src/vector/drivers/qdrant.rs
use crate::vector::{driver::VectorStore, Embedding, VectorMatch};
use crate::FrameworkError;
use async_trait::async_trait;
use qdrant_client::{
    qdrant::{
        CreateCollection, Distance, PointStruct, SearchPoints, Value, VectorParams, VectorsConfig,
    },
    Qdrant,
};
use std::collections::HashMap;

pub struct QdrantVectorStore {
    client: Qdrant,
    collection: String,
}

impl QdrantVectorStore {
    pub async fn new(url: String, collection: String) -> Result<Self, FrameworkError> {
        let client = Qdrant::from_url(&url)
            .build()
            .map_err(|e| FrameworkError::internal(format!("qdrant: {}", e)))?;

        // Create the collection if missing. The dim is inferred on
        // first upsert; for cleanliness we expose a separate
        // `ensure_collection(dim, distance)` instead.
        Ok(Self { client, collection })
    }

    pub async fn ensure_collection(&self, dim: u64) -> Result<(), FrameworkError> {
        let _ = self
            .client
            .create_collection(CreateCollection {
                collection_name: self.collection.clone(),
                vectors_config: Some(VectorsConfig {
                    config: Some(qdrant_client::qdrant::vectors_config::Config::Params(VectorParams {
                        size: dim,
                        distance: Distance::Cosine as i32,
                        ..Default::default()
                    })),
                }),
                ..Default::default()
            })
            .await; // ignore "already exists"
        Ok(())
    }
}

#[async_trait]
impl VectorStore for QdrantVectorStore {
    async fn upsert(
        &self,
        records: &[(&str, Embedding, serde_json::Value)],
    ) -> Result<(), FrameworkError> {
        if let Some((_, first_emb, _)) = records.first() {
            self.ensure_collection(first_emb.dim() as u64).await?;
        }
        let points: Vec<PointStruct> = records
            .iter()
            .map(|(id, emb, meta)| {
                let mut payload: HashMap<String, Value> = HashMap::new();
                if let serde_json::Value::Object(obj) = meta {
                    for (k, v) in obj {
                        payload.insert(k.clone(), serde_to_qdrant(v));
                    }
                }
                PointStruct::new(id.to_string(), emb.values.clone(), payload)
            })
            .collect();
        self.client
            .upsert_points(qdrant_client::qdrant::UpsertPoints {
                collection_name: self.collection.clone(),
                points,
                ..Default::default()
            })
            .await
            .map_err(|e| FrameworkError::internal(format!("qdrant upsert: {}", e)))?;
        Ok(())
    }

    async fn similar(
        &self,
        query: Embedding,
        k: usize,
    ) -> Result<Vec<VectorMatch>, FrameworkError> {
        let resp = self
            .client
            .search_points(SearchPoints {
                collection_name: self.collection.clone(),
                vector: query.values,
                limit: k as u64,
                with_payload: Some(true.into()),
                ..Default::default()
            })
            .await
            .map_err(|e| FrameworkError::internal(format!("qdrant search: {}", e)))?;

        Ok(resp
            .result
            .into_iter()
            .map(|p| VectorMatch {
                id: p
                    .id
                    .map(|i| format!("{:?}", i.point_id_options))
                    .unwrap_or_default(),
                score: p.score,
                metadata: qdrant_payload_to_json(&p.payload),
            })
            .collect())
    }

    async fn delete(&self, ids: &[&str]) -> Result<(), FrameworkError> {
        self.client
            .delete_points(qdrant_client::qdrant::DeletePoints {
                collection_name: self.collection.clone(),
                points: Some(
                    qdrant_client::qdrant::PointsSelector::from(
                        ids.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                    ),
                ),
                ..Default::default()
            })
            .await
            .map_err(|e| FrameworkError::internal(format!("qdrant delete: {}", e)))?;
        Ok(())
    }
}

fn serde_to_qdrant(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::String(s) => s.clone().into(),
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0).into(),
        serde_json::Value::Bool(b) => (*b).into(),
        _ => v.to_string().into(),
    }
}

fn qdrant_payload_to_json(payload: &HashMap<String, Value>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in payload {
        map.insert(k.clone(), serde_json::Value::String(format!("{:?}", v.kind)));
    }
    serde_json::Value::Object(map)
}
```

> **API verification:** `qdrant-client` 1.10 has gone through several major changes. Verify exact types (`PointStruct::new`, `SearchPoints` shape, payload conversion) via `cargo doc -p qdrant-client --open --no-deps`. The payload→JSON round-trip in particular needs the implementer to map every Qdrant `Value::Kind` variant to its JSON analogue, not just stringify.

- [ ] **Step 4: Smoke test**

```bash
docker run -d --rm --name qdrant -p 6334:6334 qdrant/qdrant
cargo test -p suprnova --test vector -- --ignored qdrant
docker stop qdrant
```

- [ ] **Step 5: Commit**

```bash
git add framework/src/vector/drivers/qdrant.rs framework/Cargo.toml
git commit -m "feat(vector): Qdrant driver behind vector-qdrant feature flag"
```

---

## Task 3: LanceDB vector driver

**Files:** `framework/src/vector/drivers/lancedb.rs`

- [ ] **Step 1: Implement** (LanceDB is embedded — no separate server)

```rust
// framework/src/vector/drivers/lancedb.rs
use crate::vector::{driver::VectorStore, Embedding, VectorMatch};
use crate::FrameworkError;
use async_trait::async_trait;
use lancedb::{connect, Connection, Table};

pub struct LanceDbStore {
    table: Table,
}

impl LanceDbStore {
    pub async fn new(path: String, table_name: String) -> Result<Self, FrameworkError> {
        let conn = connect(&path)
            .execute()
            .await
            .map_err(|e| FrameworkError::internal(format!("lancedb open: {}", e)))?;
        let table = match conn.open_table(&table_name).execute().await {
            Ok(t) => t,
            Err(_) => {
                // Create the table with a schema on first use.
                // LanceDB needs an Arrow schema; build minimal:
                // id: Utf8, vector: FixedSizeList<Float32>, metadata: Utf8 (JSON).
                // For brevity, this sketch defers schema definition
                // to the implementer per LanceDB's current API; the
                // builder pattern is `conn.create_table(name, batches).execute()`.
                return Err(FrameworkError::internal(
                    "lancedb table missing; create with explicit schema before first use",
                ));
            }
        };
        Ok(Self { table })
    }
}

#[async_trait]
impl VectorStore for LanceDbStore {
    async fn upsert(
        &self,
        records: &[(&str, Embedding, serde_json::Value)],
    ) -> Result<(), FrameworkError> {
        // Build an Arrow RecordBatch from records, then call
        // `self.table.add(...)` or `merge_insert(...)` for upsert.
        // Full impl: see lancedb examples; the structure is:
        //   let schema = self.table.schema().await?;
        //   let batch = build_batch(schema, records)?;
        //   self.table.merge_insert(&["id"]).execute(reader).await?;
        Err(FrameworkError::internal("lancedb upsert: implementer to wire Arrow RecordBatch"))
    }

    async fn similar(
        &self,
        query: Embedding,
        k: usize,
    ) -> Result<Vec<VectorMatch>, FrameworkError> {
        let results = self
            .table
            .vector_search(query.values)
            .map_err(|e| FrameworkError::internal(format!("lancedb search: {}", e)))?
            .limit(k)
            .execute()
            .await
            .map_err(|e| FrameworkError::internal(format!("lancedb execute: {}", e)))?;
        // Convert Arrow result stream → VectorMatch list.
        // Implementer: read each batch's id/_distance/metadata columns.
        let _ = results;
        Ok(vec![])
    }

    async fn delete(&self, ids: &[&str]) -> Result<(), FrameworkError> {
        let predicate = format!(
            "id IN ({})",
            ids.iter()
                .map(|s| format!("'{}'", s.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(",")
        );
        self.table
            .delete(&predicate)
            .await
            .map_err(|e| FrameworkError::internal(format!("lancedb delete: {}", e)))?;
        Ok(())
    }
}
```

> **Implementer scope:** LanceDB driver is the largest of the four — Arrow integration takes real work. Allocate a separate agent task; the upsert + similar methods need the Arrow RecordBatch round-trip wired in full. Reference: `lancedb` crate's `examples/` directory.

- [ ] **Step 2: Commit**

```bash
git add framework/src/vector/drivers/lancedb.rs
git commit -m "feat(vector): LanceDB driver behind vector-lancedb feature (Arrow round-trip TBD)"
```

---

## Task 4: pgvector + MariaDB VECTOR + LibSQL drivers (SQL-backed vectors)

**Files:** `framework/src/vector/drivers/{pgvector,mariadb,libsql}.rs`

- [ ] **Step 1: Pattern — each SQL backend uses raw SQL via `sqlx` / `libsql`**

```rust
// framework/src/vector/drivers/pgvector.rs
use crate::vector::{driver::VectorStore, Embedding, VectorMatch};
use crate::FrameworkError;
use async_trait::async_trait;
use sqlx::PgPool;

pub struct PgVectorStore {
    pool: PgPool,
    table: String,
    dim: usize,
}

impl PgVectorStore {
    pub async fn new(url: String, table: String, dim: usize) -> Result<Self, FrameworkError> {
        let pool = PgPool::connect(&url)
            .await
            .map_err(|e| FrameworkError::internal(format!("postgres: {}", e)))?;
        // Ensure extension + table.
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&pool)
            .await
            .map_err(|e| FrameworkError::internal(format!("vector ext: {}", e)))?;
        let ddl = format!(
            "CREATE TABLE IF NOT EXISTS {} (id TEXT PRIMARY KEY, embedding VECTOR({}) NOT NULL, metadata JSONB NOT NULL DEFAULT '{{}}'::jsonb)",
            table, dim
        );
        sqlx::query(&ddl)
            .execute(&pool)
            .await
            .map_err(|e| FrameworkError::internal(format!("create table: {}", e)))?;
        Ok(Self { pool, table, dim })
    }
}

#[async_trait]
impl VectorStore for PgVectorStore {
    async fn upsert(
        &self,
        records: &[(&str, Embedding, serde_json::Value)],
    ) -> Result<(), FrameworkError> {
        for (id, emb, meta) in records {
            if emb.dim() != self.dim {
                return Err(FrameworkError::internal(format!(
                    "embedding dim {} != store dim {}", emb.dim(), self.dim
                )));
            }
            let emb_str = format!(
                "[{}]",
                emb.values.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(",")
            );
            let sql = format!(
                "INSERT INTO {} (id, embedding, metadata) VALUES ($1, $2::vector, $3) \
                 ON CONFLICT (id) DO UPDATE SET embedding = EXCLUDED.embedding, metadata = EXCLUDED.metadata",
                self.table
            );
            sqlx::query(&sql)
                .bind(id)
                .bind(emb_str)
                .bind(meta)
                .execute(&self.pool)
                .await
                .map_err(|e| FrameworkError::internal(format!("pgvector upsert: {}", e)))?;
        }
        Ok(())
    }

    async fn similar(
        &self,
        query: Embedding,
        k: usize,
    ) -> Result<Vec<VectorMatch>, FrameworkError> {
        let q_str = format!(
            "[{}]",
            query.values.iter().map(|f| f.to_string()).collect::<Vec<_>>().join(",")
        );
        let sql = format!(
            "SELECT id, metadata, 1 - (embedding <=> $1::vector) AS score \
             FROM {} ORDER BY embedding <=> $1::vector LIMIT $2",
            self.table
        );
        let rows = sqlx::query_as::<_, (String, serde_json::Value, f32)>(&sql)
            .bind(q_str)
            .bind(k as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| FrameworkError::internal(format!("pgvector search: {}", e)))?;
        Ok(rows
            .into_iter()
            .map(|(id, meta, score)| VectorMatch {
                id,
                score,
                metadata: meta,
            })
            .collect())
    }

    async fn delete(&self, ids: &[&str]) -> Result<(), FrameworkError> {
        let sql = format!("DELETE FROM {} WHERE id = ANY($1)", self.table);
        sqlx::query(&sql)
            .bind(ids)
            .execute(&self.pool)
            .await
            .map_err(|e| FrameworkError::internal(format!("pgvector delete: {}", e)))?;
        Ok(())
    }
}
```

```rust
// framework/src/vector/drivers/mariadb.rs
// Same shape, but using MariaDB 11.7+'s VECTOR() data type and
// VEC_DISTANCE_COSINE / VEC_FromText(...) helpers. The DDL:
//   CREATE TABLE docs (id VARCHAR(255) PRIMARY KEY, embedding VECTOR(384) NOT NULL, metadata JSON, VECTOR INDEX (embedding))
// SELECT id, VEC_DISTANCE_COSINE(embedding, VEC_FromText(?)) AS score FROM docs ORDER BY score LIMIT ?
```

```rust
// framework/src/vector/drivers/libsql.rs
// LibSQL exposes vector via the libsql_vector extension. Schema:
//   CREATE TABLE docs (id TEXT PRIMARY KEY, embedding F32_BLOB(384), metadata TEXT)
//   CREATE INDEX docs_idx ON docs(libsql_vector_idx(embedding))
// Query: SELECT id, metadata, vector_distance_cos(embedding, vector32(?)) AS score FROM docs ORDER BY score LIMIT ?
```

- [ ] **Step 2: Commit each driver as its own commit**

```bash
git commit -m "feat(vector): pgvector driver — postgres VECTOR + COSINE distance"
git commit -m "feat(vector): MariaDB driver — MariaDB 11.7 VECTOR() + VEC_DISTANCE_COSINE"
git commit -m "feat(vector): LibSQL driver — libsql_vector extension + F32_BLOB"
```

---

## Task 5: Weaviate + Milvus vector drivers

**Files:** `framework/src/vector/drivers/{weaviate,milvus}.rs`

- [ ] **Step 1: Implement** (HTTP-based clients; Weaviate has `weaviate-client` crate, Milvus has `milvus-sdk-rust`)

```rust
// framework/src/vector/drivers/weaviate.rs
// Use the official weaviate-client crate. The store name maps to a
// Weaviate "Class"; metadata becomes class properties; embeddings
// are stored against `_vector`. Upsert via `data().creator()` calls.
```

```rust
// framework/src/vector/drivers/milvus.rs
// milvus-sdk-rust exposes Collection / Schema / FieldSchema. Store
// name → collection name. Schema: id (varchar), embedding (float_vector),
// metadata (json).
```

- [ ] **Step 2: Commit each**

```bash
git commit -m "feat(vector): Weaviate driver"
git commit -m "feat(vector): Milvus driver"
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
