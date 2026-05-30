# Vector

Suprnova ships a Laravel-shape `Vector` facade backed by one of four
drivers — in-process Memory, Qdrant, Pinecone, or MariaDB native
`VECTOR(N)` — picked explicitly at boot via `Vector::register`. The
facade is a thin layer over a `VectorDriver` trait, so custom backends
plug in the same way the built-ins do.

## Quickstart

```rust
use std::sync::Arc;
use suprnova::{MemoryVectorDriver, Vector, VectorItem};

// Bootstrap (typically once at app start)
Vector::register("documents", Arc::new(MemoryVectorDriver::new()));

// Use it
let store = Vector::store("documents")?;
store
    .upsert(vec![
        VectorItem::new("doc-1", embedding_for("Hello"), serde_json::json!({ "title": "Hello" })),
        VectorItem::new("doc-2", embedding_for("World"), serde_json::json!({ "title": "World" })),
    ])
    .await?;

let hits = store.similar(query_embedding, 10).await?;
for hit in hits {
    println!("{}: {} (score {:.3})", hit.id, hit.metadata["title"], hit.score);
}
```

## The contract

```rust
#[async_trait]
pub trait VectorDriver: Send + Sync + 'static {
    async fn upsert(&self, store: &str, items: Vec<VectorItem>) -> Result<(), FrameworkError>;
    async fn similar(&self, store: &str, query: Vec<f32>, k: usize) -> Result<Vec<VectorMatch>, FrameworkError>;
    async fn delete(&self, store: &str, ids: Vec<String>) -> Result<(), FrameworkError>;
    async fn count(&self, store: &str) -> Result<usize, FrameworkError>;
}
```

`VectorItem` carries an arbitrary `String` id, an `embedding: Vec<f32>`, and freeform `metadata: serde_json::Value` (must be a JSON object or `null`). `VectorMatch` returns the original id, the backend's similarity score, and the same metadata shape.

The trait is intentionally small. When you need filter expressions on search, sparse vectors, scroll/list, snapshots, or quantization knobs, drop down to the driver's underlying SDK via its public `client()` trapdoor.

### Why Suprnova diverges

Laravel ships vectors only through Postgres `pgvector`. That's the
PHP-shape answer: pick one storage backend, hide it behind a single
driver, and call it done. Suprnova treats the choice as a configuration
concern. The same trait covers an in-process `HashMap` for tests,
a dedicated vector DB (Qdrant, Pinecone) when the embedding count
justifies the operational cost, and a relational backend (MariaDB
11.7+) when you'd rather keep vectors next to the rows that produced
them. Weaviate, Milvus, LanceDB, pgvector, and LibSQL queue up behind
real consumer demand — none are blocked by the trait shape.

When the rest of your app fits on one engine, MariaDB 11.7+ keeps
vectors alongside relational tables, JSON documents, and
system-versioned temporal data — fewer moving parts than running
Postgres + Redis + Qdrant separately. See [Deployment](deployment.md)
for the recommendation in context.

## Drivers

### Memory — `MemoryVectorDriver`

In-process driver backed by `HashMap`. Cosine similarity, dimension-mismatch points are silently skipped on query (so mixed-dim test data doesn't blow up), zero-vector queries error clearly.

```rust
Vector::register("docs", Arc::new(MemoryVectorDriver::new()));
```

Use in tests and dev. Each `MemoryVectorDriver::new()` instance is hermetic — no shared state between two new()s.

### Qdrant — `QdrantVectorDriver`

Talks to Qdrant over gRPC (default port 6334) via the official `qdrant-client` SDK.

```rust
use suprnova::{QdrantDistance, QdrantVectorDriver};

let driver = QdrantVectorDriver::from_url("http://localhost:6334")?
    .with_distance(QdrantDistance::Cosine)  // default
    .with_auto_create(true);                // default

Vector::register("docs", Arc::new(driver));
```

For Qdrant Cloud:

```rust
let driver = QdrantVectorDriver::from_url_with_api_key(
    "https://xxxxxxxx.eu-central.aws.cloud.qdrant.io:6334",
    std::env::var("QDRANT_API_KEY")?,
)?;
```

**ID mapping.** Qdrant requires point IDs to be either `u64` or a valid UUID. The framework bridges arbitrary strings with three rules:

1. If the string parses as `u64`, use the `Num(u64)` variant.
2. If the string is a valid UUID, use the `Uuid(String)` variant verbatim.
3. Otherwise, derive a deterministic v5 UUID from a stable namespace.

The caller's original string is stashed in the point's payload under the reserved key `__suprnova_id` (exported as `SUPRNOVA_ID_PAYLOAD_KEY`) and stripped from `VectorMatch.metadata` on retrieval. Power users who query Qdrant directly via `driver.client()` can filter on `__suprnova_id` to bridge framework writes with direct calls.

**Auto-create.** On first `upsert` for an unseen collection, the driver creates it with the dimension inferred from the first item and the configured distance metric (Cosine by default). Race-safe — concurrent upserters on the same fresh collection won't fail; whichever creates first wins, the other proceeds. Disable via `.with_auto_create(false)` to require explicit creation.

**Cache invalidation.** If a collection is dropped externally (or Qdrant restarts before persistence flushed), the driver detects the "not found" error on upsert, drops the cache entry, re-runs `ensure_collection`, and retries once.

**Trapdoor.** `driver.client()` returns the underlying `qdrant_client::Qdrant` — use it for filter expressions on search, scroll, snapshots, or other APIs not surfaced via the trait. `QdrantVectorDriver::resolve_point_id`, `build_point`, and `decode_match` let you mix direct and trait-routed calls without losing id translation.

**Local setup.** Run Qdrant via Docker:

```bash
docker run -p 6334:6334 -p 6333:6333 qdrant/qdrant
```

Integration tests run via:

```bash
QDRANT_URL=http://localhost:6334 cargo test -p suprnova --test vector_qdrant -- --ignored
```

### Pinecone — `PineconeVectorDriver`

> **Feature-gated — off by default.** Enable with `cargo build --features vector-pinecone` (or add `features = ["vector-pinecone"]` under the `suprnova` dep in your `Cargo.toml`). The gate exists because `pinecone-sdk 0.1.2` (the latest on crates.io) pins `tonic 0.11.0`, which pulls four active rustls-webpki RustSec advisories (`RUSTSEC-2026-0049`, `-0098`, `-0099`, `-0104`). Default builds stay clean; consumers who need Pinecone opt in explicitly and accept the dep chain.

Talks to Pinecone over gRPC via the official `pinecone-sdk` crate.

```rust
use suprnova::PineconeVectorDriver;

// API key directly
let driver = PineconeVectorDriver::from_api_key(std::env::var("PINECONE_API_KEY")?)?;

// Or via env (uses the SDK's PINECONE_API_KEY env contract)
let driver = PineconeVectorDriver::from_env()?;

// Bind to a non-default namespace
let driver = driver.with_namespace("public");

Vector::register("docs", Arc::new(driver));
```

The store name passed via `Vector::store(name)` maps to a Pinecone index name. The driver lazily resolves the index host via `describe_index` on first use and caches the resulting `Index` handle.

**No auto-create.** Pinecone index creation requires picking cloud (AWS/GCP/Azure), region, vector dimension, distance metric, and deletion-protection — too many trade-offs to default well. Create indexes via the Pinecone console or via the underlying client (`driver.client().create_serverless_index(...)`) before registering, then point the framework at the existing name.

This is the principal asymmetry with the Qdrant driver, which auto-creates collections on first upsert.

**IDs and metadata.** Pinecone accepts arbitrary `String` ids natively, so `VectorItem::id` passes straight through. Metadata bridges `serde_json::Value` ↔ `prost_types::Struct` via `PineconeVectorDriver::json_to_metadata` / `metadata_to_json`. Pinecone stores numbers as `f64` — that's a Pinecone constraint, not a framework one.

**Namespaces.** One driver instance binds to one namespace. To use multiple namespaces of the same index, register one driver per namespace under different store names:

```rust
Vector::register("docs-public", Arc::new(
    PineconeVectorDriver::from_env()?.with_namespace("public")
));
Vector::register("docs-private", Arc::new(
    PineconeVectorDriver::from_env()?.with_namespace("private")
));
```

**Throughput.** v1 caches one `Index` per index name behind a `tokio::Mutex`. Calls to the same Pinecone index serialize through that mutex — a pragmatic limitation because pinecone-sdk exposes `Index` only behind `&mut self`. For higher throughput register multiple driver instances or call `driver.client()` directly.

**Trapdoor.** `driver.client()` returns the underlying `PineconeClient` for filter expressions on query, sparse vectors, multi-namespace queries, and index management.

**Integration tests** require both env vars:

```bash
PINECONE_API_KEY=... PINECONE_TEST_INDEX=my-test-index \
    cargo test -p suprnova --test vector_pinecone -- --ignored
```

### MariaDB — `MariaDbVectorDriver`

Talks to MariaDB 11.7+ via direct `sqlx::MySqlPool`, using MariaDB's native `VECTOR(N)` column type and HNSW indexing. The first time you call a driver method, it runs `SELECT VERSION()` and rejects anything below 11.7 — older servers don't have the vector functions.

```rust
use std::sync::Arc;
use suprnova::{MariaDbDistance, MariaDbVectorDriver, Vector};

let driver = MariaDbVectorDriver::from_url(
    "mysql://user:pass@localhost:3306/myapp",
)?
.with_distance(MariaDbDistance::Cosine);  // default

Vector::register("documents", Arc::new(driver));
```

`from_url` is lazy — it validates the URL syntax but does NOT open a connection until first use, so calling it at app bootstrap is safe even before the database is reachable. Wrap an existing pool with `MariaDbVectorDriver::from_pool(pool)` when you need custom pool options.

**Schema is yours.** The driver does not auto-create tables — schema is a migration concern. The recommended path is `driver.ensure_table_sql_for(name, dim)`, which inherits the driver's configured distance so the migration's `DISTANCE=` clause and the query function `similar` uses are guaranteed to match:

```rust
let driver = MariaDbVectorDriver::from_url(url)?
    .with_distance(MariaDbDistance::Cosine);

let sql = driver.ensure_table_sql_for("documents", 1536)?;
// Result:
// CREATE TABLE IF NOT EXISTS `documents` (
//   id VARCHAR(255) NOT NULL PRIMARY KEY,
//   embedding VECTOR(1536) NOT NULL,
//   metadata JSON NULL,
//   VECTOR INDEX (embedding) DISTANCE=cosine
// ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4
```

For migration generators that don't have a driver in scope (CLI tools, build scripts), use the static `MariaDbVectorDriver::ensure_table_sql(name, dim, distance)` and pass the same `MariaDbDistance` you'll later configure on the driver.

**Distance must match on both ends.** MariaDB silently falls back to a full table scan when the function used at query time doesn't match the index's `DISTANCE=` clause. The driver guards against this in two layers:

1. **`ensure_table_sql_for(name, dim)`** reads `self.distance` for both the emitted migration SQL and the runtime function in `similar` — they cannot drift apart by construction.
2. **A runtime check on first `similar` call** runs one `SHOW CREATE TABLE` per store, parses the actual `DISTANCE=` clause from the live schema, and errors clearly if it disagrees with `with_distance(...)`. Result is cached, so subsequent calls are zero-cost. This catches hand-written migrations or `from_pool` setups that bypass `ensure_table_sql_for`.

**Store-name safety.** Store names interpolate into emitted SQL (MySQL doesn't parameterize identifiers). Names are validated as `[A-Za-z_][A-Za-z0-9_]*` of length ≤ 64; the validated name is then backtick-quoted in every statement. Invalid names error with `FrameworkError::param` at the `register`/`upsert`/`similar`/`delete`/`count` boundary.

**IDs and metadata.** `VARCHAR(255)` accepts arbitrary `String` ids — no UUID derivation, no reserved payload keys. Metadata round-trips through MariaDB's `JSON` column type; `null` metadata stores as SQL `NULL`. Non-object metadata (arrays, primitives) is rejected with `FrameworkError::param` for parity with Qdrant and Pinecone.

**Score normalization.** MariaDB returns raw *distance* (lower = closer). The trait contract is *score* (higher = more similar) — the driver converts per metric:

| Metric    | MariaDB returns       | Exposed `score`              |
| --------- | --------------------- | ---------------------------- |
| Cosine    | `[0, 2]` (`1 - cos`)  | `1.0 - d / 2.0` → `[0, 1]`   |
| Euclidean | `[0, ∞)` L2 norm      | `1.0 / (1.0 + d)` → `(0, 1]` |

In both cases, ranking is preserved (best result first); the score is comparable across drivers because every backend lands on the same `higher = better` convention.

**Trapdoor.** `driver.pool()` returns the underlying `sqlx::MySqlPool` for raw queries the trait doesn't cover. `MariaDbVectorDriver::embedding_to_vec_text`, `score_from_distance`, and `ensure_table_sql` are pure functions you can call independently when mixing direct SQL with trait-routed calls.

**Bulk upsert behavior.** `upsert` emits one multi-row `INSERT ... VALUES (...), (...), ...` statement per 500-row chunk, all wrapped in a single transaction. Network round-trips drop ~500x vs per-row inserts when loading a fresh corpus; the call stays atomic across the whole batch. The batch size is internal — call `upsert` once with all your items and the driver handles chunking.

**HNSW indexes rebuild at commit time.** MariaDB updates the HNSW graph as rows go in, but the index work concentrates at commit. A 1M-row `upsert` will hold the transaction open for the full duration of the index build, which can be minutes. For very large initial loads, break the corpus into 10k–100k-row batches and call `upsert` repeatedly so each batch commits and frees the lock between rounds. (Smaller `upsert` calls are not slower per row — they just spread the index work into more commit points.)

**Dimension is pinned at table creation.** `VECTOR(N)` fixes the dimension; switching embedding models from a 768-dim model to a 1536-dim model means a full table migration (new table, re-embed, swap). Plan model upgrades the same way you'd plan a schema migration — there is no "ALTER COLUMN VECTOR(768) → VECTOR(1536)" path.

**Pool sizing.** `from_url` uses sqlx's default `MySqlPoolOptions` — `max_connections = 10` at the time of writing. For high-QPS workloads (hundreds of `similar` calls per second), build the pool yourself with `MySqlPoolOptions::new().max_connections(N).connect_lazy(url)` and pass to `from_pool`. The driver doesn't impose its own connection cap.

**Local setup.** Run MariaDB 11.7+ via Docker:

```bash
docker run -p 3306:3306 \
    -e MARIADB_ROOT_PASSWORD=secret \
    -e MARIADB_DATABASE=vectors \
    mariadb:11.7
```

Integration tests run via:

```bash
MARIADB_URL='mysql://root:secret@localhost:3306/vectors' \
    cargo test -p suprnova --test vector_mariadb -- --ignored
```

## Driver comparison

| Aspect | Memory | Qdrant | Pinecone | MariaDB |
| --- | --- | --- | --- | --- |
| Backing store | `HashMap` | Qdrant gRPC | Pinecone gRPC | MariaDB SQL |
| Persistence | None | Yes | Yes | Yes |
| Auto-create | n/a | Yes (configurable) | No (user creates index) | No (migration is yours) |
| String IDs | Native | Hashed to UUID-5 | Native | Native |
| Metadata key reserved | None | `__suprnova_id` | None | None |
| Throughput | Per-process | Concurrent | Serialized per index (v1) | Concurrent (pool-bounded) |
| Distance metric | Cosine | Configurable | Set at index creation | Cosine / Euclidean |
| Version requirement | — | Any | Any | **11.7+** |

## Operational notes

**Store name conventions.** The store name passed to `Vector::register` and `Vector::store` is a label — it can be any string. For Qdrant the framework uses it as the collection name; for Pinecone as the index name. Match the label to the backend's existing naming scheme.

**Re-registering** a name with a new driver instance is a last-write-wins operation by design — useful for swapping drivers in test harnesses without restarting the process.

**Test isolation.** Both Memory and registry-backed driver tests use timestamp-tagged unique store names to avoid collisions under parallel test runs.

**Error semantics.** `Vector::store(name)` returns `FrameworkError::not_found` for unregistered names. Driver-level failures (network, auth, dimension mismatch) come back as `FrameworkError::internal` or `FrameworkError::param` with the cause string in the display message.

## Extending

To add a fifth backend (Weaviate, Milvus, LanceDB, pgvector, LibSQL, ...):

1. Add a new `framework/src/vector/<backend>.rs` implementing `VectorDriver`.
2. Re-export the driver type from `framework/src/vector/mod.rs` and the crate root.
3. Mirror the Qdrant/Pinecone test split: pure-function tests always run, integration tests `#[ignore]`-gated behind env vars for credentials.

The trait is intentionally small so the bar to ship a new driver stays low. If a backend needs surface that doesn't fit (filter expressions, sparse vectors, hybrid search), expose it via the driver's `client()` trapdoor — don't bloat the trait.

## Next

- [Deployment](deployment.md) — the MariaDB-as-default-production
  recommendation in context
- [Database](database.md) — multi-driver SeaORM setup, including
  MariaDB as a relational backend alongside vectors
- [Environment Variables](env-vars.md) — `QDRANT_URL`,
  `PINECONE_API_KEY`, `MARIADB_URL` and other driver env contracts
- [Cache](cache.md) — sibling facade with the same driver-trait shape
- [Laravel Parity Map](parity.md) — where vector search sits relative
  to Scout
