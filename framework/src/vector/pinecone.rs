//! Phase 9A — Pinecone vector driver via the official `pinecone-sdk` crate.
//!
//! Talks to Pinecone over gRPC (data plane) and HTTPS (control plane).
//! A thin adapter that satisfies [`VectorDriver`] while preserving the
//! framework's `String` IDs and `serde_json::Value` payload contract.
//!
//! Construct via [`PineconeVectorDriver::from_api_key`] (or via env
//! through [`PineconeVectorDriver::from_env`], which uses the SDK's
//! own `PINECONE_API_KEY` env contract). The driver targets one
//! Pinecone account; each `store` name passed via the trait surface
//! maps to a single Pinecone index inside that account. Index hosts
//! are resolved lazily on first use via `describe_index`, then cached.
//!
//! # ID mapping — there is none
//!
//! Unlike Qdrant, Pinecone accepts arbitrary `String` ids natively.
//! [`VectorItem::id`] passes through to Pinecone unchanged; similarity
//! hits return that same string in [`VectorMatch::id`]. No reserved
//! payload keys, no derived UUIDs.
//!
//! # Namespaces
//!
//! Pinecone indexes carry namespaces (multi-tenant partitions inside
//! one index). One driver instance binds to one namespace
//! (default: empty, i.e. the unnamed namespace) — set via
//! [`PineconeVectorDriver::with_namespace`]. To target several
//! namespaces of the same index, register one driver per namespace
//! under different store names:
//!
//! ```rust,ignore
//! Vector::register("docs-public", Arc::new(
//!     PineconeVectorDriver::from_env()?.with_namespace("public")
//! ));
//! Vector::register("docs-private", Arc::new(
//!     PineconeVectorDriver::from_env()?.with_namespace("private")
//! ));
//! ```
//!
//! # Index creation
//!
//! The driver does **not** auto-create indexes. Pinecone index
//! creation requires picking a cloud (AWS/GCP/Azure), region, vector
//! dimension, distance metric, and deletion-protection setting — too
//! many trade-offs to default well. Create the index via the
//! Pinecone console, the Pinecone CLI, or the [`PineconeClient`]
//! exposed through [`PineconeVectorDriver::client`], then register
//! the driver with Suprnova.
//!
//! [`PineconeClient`]: pinecone_sdk::pinecone::PineconeClient
//!
//! # Trapdoor
//!
//! When you outgrow the trait surface — filter expressions, sparse
//! vectors, multi-namespace queries, index management — drop down to
//! [`PineconeVectorDriver::client`] for the underlying
//! `pinecone_sdk::pinecone::PineconeClient`. The [`PineconeVectorDriver::namespace`]
//! and [`PineconeVectorDriver::json_to_metadata`] /
//! [`PineconeVectorDriver::metadata_to_json`] helpers let you stay
//! consistent with the framework's namespace and payload conventions
//! when mixing direct and trait-routed calls.
//!
//! # Throughput note
//!
//! v1 caches one `Index` per index name behind a `tokio::Mutex`. Calls
//! to the same Pinecone index serialize through that mutex. This is a
//! pragmatic v1 limitation (the SDK exposes `Index` only behind
//! `&mut self`); if throughput against a single index becomes a
//! concern, register multiple driver instances with different
//! Pinecone API keys or use [`PineconeVectorDriver::client`] directly.

use super::driver::{VectorDriver, VectorItem, VectorMatch};
use crate::FrameworkError;
use async_trait::async_trait;
use pinecone_sdk::models::{Namespace, Vector as PineconeVector};
use pinecone_sdk::pinecone::data::Index;
use pinecone_sdk::pinecone::{PineconeClient, PineconeClientConfig};
use prost_types::{value::Kind, Struct as PbStruct, Value as PbValue};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// Pinecone-backed [`VectorDriver`].
pub struct PineconeVectorDriver {
    client: Arc<PineconeClient>,
    namespace: Namespace,
    indices: Arc<RwLock<HashMap<String, Arc<Mutex<Index>>>>>,
}

impl PineconeVectorDriver {
    /// Wrap an already-built `PineconeClient`. Useful when you need
    /// custom control-plane configuration (alternative controller host,
    /// extra headers).
    pub fn from_client(client: PineconeClient) -> Self {
        Self {
            client: Arc::new(client),
            namespace: Namespace::default(),
            indices: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Construct against Pinecone with an explicit API key.
    pub fn from_api_key(api_key: impl Into<String>) -> Result<Self, FrameworkError> {
        let config = PineconeClientConfig {
            api_key: Some(api_key.into()),
            ..Default::default()
        };
        let client = config
            .client()
            .map_err(|e| FrameworkError::internal(format!("pinecone client init: {e}")))?;
        Ok(Self::from_client(client))
    }

    /// Construct using the SDK's own env-var contract: reads
    /// `PINECONE_API_KEY` (required) and optionally
    /// `PINECONE_CONTROLLER_HOST`.
    pub fn from_env() -> Result<Self, FrameworkError> {
        let client = pinecone_sdk::pinecone::default_client()
            .map_err(|e| FrameworkError::internal(format!("pinecone client init from env: {e}")))?;
        Ok(Self::from_client(client))
    }

    /// Bind this driver to a non-default namespace.
    pub fn with_namespace(mut self, name: impl Into<String>) -> Self {
        self.namespace = Namespace {
            name: name.into(),
        };
        self
    }

    /// Borrow the underlying `PineconeClient`. Use this when the
    /// trait surface isn't enough — filter expressions on query,
    /// sparse vectors, index management, etc.
    pub fn client(&self) -> &PineconeClient {
        &self.client
    }

    /// The namespace this driver binds writes and queries to.
    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    /// Convert a JSON object/null into Pinecone's protobuf-shaped
    /// metadata. Returns a `param` error for any non-object,
    /// non-null JSON value. Exposed so power users mixing direct
    /// `pinecone_sdk::pinecone::data::Index` calls with framework
    /// upserts can produce identical metadata shapes.
    pub fn json_to_metadata(value: serde_json::Value) -> Result<Option<PbStruct>, FrameworkError> {
        match value {
            serde_json::Value::Null => Ok(None),
            serde_json::Value::Object(map) => Ok(Some(PbStruct {
                fields: map
                    .into_iter()
                    .map(|(k, v)| (k, json_value_to_pb(v)))
                    .collect(),
            })),
            other => Err(FrameworkError::param(format!(
                "vector metadata must be a JSON object or null, got: {other}"
            ))),
        }
    }

    /// Convert Pinecone's protobuf-shaped metadata back into JSON.
    /// `None` becomes `serde_json::Value::Null` for symmetry with
    /// [`Self::json_to_metadata`].
    pub fn metadata_to_json(metadata: Option<PbStruct>) -> serde_json::Value {
        match metadata {
            None => serde_json::Value::Null,
            Some(s) => serde_json::Value::Object(
                s.fields
                    .into_iter()
                    .map(|(k, v)| (k, pb_value_to_json(v)))
                    .collect(),
            ),
        }
    }

    /// Convert a [`VectorItem`] into Pinecone's `Vector` wire type.
    /// Pure-function helper exposed for power users.
    pub fn build_vector(item: VectorItem) -> Result<PineconeVector, FrameworkError> {
        let metadata = Self::json_to_metadata(item.metadata)?;
        Ok(PineconeVector {
            id: item.id,
            values: item.embedding,
            sparse_values: None,
            metadata,
        })
    }

    /// Decode a Pinecone scored match into a framework-side
    /// [`VectorMatch`]. Takes the three fields individually because
    /// `pinecone-sdk` 0.1.2 does not re-export the `ScoredVector`
    /// proto type publicly. Consumers iterating
    /// `QueryResponse::matches` directly can still call this helper to
    /// keep id / score / metadata decoding consistent with the
    /// framework's contract.
    pub fn decode_match_fields(
        id: String,
        score: f32,
        metadata: Option<PbStruct>,
    ) -> VectorMatch {
        VectorMatch {
            id,
            score,
            metadata: Self::metadata_to_json(metadata),
        }
    }

    async fn acquire_index(&self, name: &str) -> Result<Arc<Mutex<Index>>, FrameworkError> {
        {
            let read = self.indices.read().await;
            if let Some(idx) = read.get(name) {
                return Ok(idx.clone());
            }
        }
        let mut write = self.indices.write().await;
        if let Some(idx) = write.get(name) {
            return Ok(idx.clone());
        }
        let description = self
            .client
            .describe_index(name)
            .await
            .map_err(|e| FrameworkError::internal(format!("pinecone describe_index '{name}': {e}")))?;
        let index = self
            .client
            .index(&description.host)
            .await
            .map_err(|e| FrameworkError::internal(format!("pinecone open index '{name}' at host '{}': {e}", description.host)))?;
        let handle = Arc::new(Mutex::new(index));
        write.insert(name.to_string(), handle.clone());
        Ok(handle)
    }
}

#[async_trait]
impl VectorDriver for PineconeVectorDriver {
    async fn upsert(&self, store: &str, items: Vec<VectorItem>) -> Result<(), FrameworkError> {
        if items.is_empty() {
            return Ok(());
        }
        let dim = items[0].embedding.len();
        if dim == 0 {
            return Err(FrameworkError::param(
                "vector::upsert items have zero-length embedding",
            ));
        }
        let vectors: Vec<PineconeVector> = items
            .into_iter()
            .map(Self::build_vector)
            .collect::<Result<_, _>>()?;
        let handle = self.acquire_index(store).await?;
        let mut index = handle.lock().await;
        index
            .upsert(&vectors, &self.namespace)
            .await
            .map_err(|e| FrameworkError::internal(format!("pinecone upsert into '{store}': {e}")))?;
        Ok(())
    }

    async fn similar(
        &self,
        store: &str,
        query: Vec<f32>,
        k: usize,
    ) -> Result<Vec<VectorMatch>, FrameworkError> {
        if k == 0 {
            return Ok(Vec::new());
        }
        if query.is_empty() {
            return Err(FrameworkError::param("vector::similar query is empty"));
        }
        let q_norm: f32 = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        if q_norm == 0.0 {
            return Err(FrameworkError::param("vector::similar query is zero-vector"));
        }
        let top_k = u32::try_from(k).map_err(|_| {
            FrameworkError::param(format!("vector::similar k={k} exceeds Pinecone's u32 limit"))
        })?;

        let handle = self.acquire_index(store).await?;
        let mut index = handle.lock().await;
        let response = index
            .query_by_value(query, None, top_k, &self.namespace, None, None, Some(true))
            .await
            .map_err(|e| FrameworkError::internal(format!("pinecone query on '{store}': {e}")))?;
        Ok(response
            .matches
            .into_iter()
            .map(|sv| Self::decode_match_fields(sv.id, sv.score, sv.metadata))
            .collect())
    }

    async fn delete(&self, store: &str, ids: Vec<String>) -> Result<(), FrameworkError> {
        if ids.is_empty() {
            return Ok(());
        }
        let handle = self.acquire_index(store).await?;
        let mut index = handle.lock().await;
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        index
            .delete_by_id(&id_refs, &self.namespace)
            .await
            .map_err(|e| FrameworkError::internal(format!("pinecone delete from '{store}': {e}")))?;
        Ok(())
    }

    async fn count(&self, store: &str) -> Result<usize, FrameworkError> {
        let handle = self.acquire_index(store).await?;
        let mut index = handle.lock().await;
        let stats = index
            .describe_index_stats(None)
            .await
            .map_err(|e| {
                FrameworkError::internal(format!(
                    "pinecone describe_index_stats on '{store}': {e}"
                ))
            })?;
        // Count is per-namespace: Pinecone returns a NamespaceSummary
        // keyed by name; the unnamed default namespace lives under
        // an empty-string key. Missing key == 0 vectors.
        Ok(stats
            .namespaces
            .get(&self.namespace.name)
            .map(|ns| ns.vector_count as usize)
            .unwrap_or(0))
    }
}

// ----------------------------------------------------------------------
// JSON <-> protobuf Value conversion. Kept pure so the public
// `json_to_metadata` / `metadata_to_json` helpers can be tested
// without spinning up a Pinecone client.
// ----------------------------------------------------------------------

fn json_value_to_pb(v: serde_json::Value) -> PbValue {
    let kind = match v {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(b) => Kind::BoolValue(b),
        serde_json::Value::Number(n) => {
            // Pinecone metadata stores numbers as f64 — Pinecone
            // itself only filters numerically on f64. Integers
            // that don't round-trip cleanly through f64 will lose
            // precision; that's a Pinecone constraint, not a
            // framework one.
            let f = n.as_f64().unwrap_or(0.0);
            Kind::NumberValue(f)
        }
        serde_json::Value::String(s) => Kind::StringValue(s),
        serde_json::Value::Array(items) => Kind::ListValue(prost_types::ListValue {
            values: items.into_iter().map(json_value_to_pb).collect(),
        }),
        serde_json::Value::Object(map) => Kind::StructValue(PbStruct {
            fields: map
                .into_iter()
                .map(|(k, v)| (k, json_value_to_pb(v)))
                .collect(),
        }),
    };
    PbValue { kind: Some(kind) }
}

fn pb_value_to_json(v: PbValue) -> serde_json::Value {
    match v.kind {
        None => serde_json::Value::Null,
        Some(Kind::NullValue(_)) => serde_json::Value::Null,
        Some(Kind::BoolValue(b)) => serde_json::Value::Bool(b),
        Some(Kind::NumberValue(n)) => serde_json::Number::from_f64(n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Some(Kind::StringValue(s)) => serde_json::Value::String(s),
        Some(Kind::ListValue(items)) => {
            serde_json::Value::Array(items.values.into_iter().map(pb_value_to_json).collect())
        }
        Some(Kind::StructValue(s)) => serde_json::Value::Object(
            s.fields
                .into_iter()
                .map(|(k, v)| (k, pb_value_to_json(v)))
                .collect(),
        ),
    }
}

