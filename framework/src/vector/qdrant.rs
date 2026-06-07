//! Phase 9A — Qdrant vector driver via the official `qdrant-client` SDK.
//!
//! Talks to Qdrant over gRPC (default port 6334). A thin adapter that
//! satisfies [`VectorDriver`] while preserving the framework's `String`
//! IDs and `serde_json::Value` payload contract.
//!
//! # ID mapping
//!
//! Qdrant requires point IDs to be either `u64` or a valid UUID. Our
//! trait accepts arbitrary `String`s. We bridge them with three rules
//! applied in order via [`QdrantVectorDriver::resolve_point_id`]:
//!
//! 1. If the string parses as `u64`, use the `Num(u64)` variant.
//! 2. If the string is a valid UUID, use the `Uuid(String)` variant
//!    verbatim.
//! 3. Otherwise, derive a deterministic v5 UUID from the framework's
//!    namespace and the bytes of the original string.
//!
//! In all three cases the original caller-side string is stashed in
//! the point's payload under [`SUPRNOVA_ID_PAYLOAD_KEY`] so similarity
//! hits can round-trip back to the caller. That key is stripped from
//! the metadata returned by [`VectorDriver::similar`] — consumers
//! never see it through the trait surface. It IS visible if you query
//! Qdrant directly (see [`QdrantVectorDriver::client`]).
//!
//! Note that this mapping is asymmetric per-id: in one collection,
//! "42" (Num) and "0e2c3d…" (Uuid) and "foo" (derived Uuid) occupy
//! disjoint id buckets. That mirrors Qdrant's native model — we
//! don't try to "fix" it.
//!
//! # Auto-create
//!
//! The driver creates a collection on the first call to
//! [`VectorDriver::upsert`] for a name it hasn't seen. The vector
//! dimension is inferred from the first item's `embedding.len()` and
//! the distance metric defaults to Cosine. Disable via
//! [`QdrantVectorDriver::with_auto_create`]; change the metric via
//! [`QdrantVectorDriver::with_distance`].
//!
//! # Trapdoor
//!
//! When you outgrow the trait surface — filter expressions, scroll,
//! quantization, multi-vector — drop down to
//! [`QdrantVectorDriver::client`] for the underlying
//! `qdrant_client::Qdrant`. [`QdrantVectorDriver::resolve_point_id`]
//! and [`QdrantVectorDriver::build_point`] /
//! [`QdrantVectorDriver::decode_match`] let you reuse the framework's
//! id+payload encoding when mixing direct and trait-routed calls.

use super::driver::{VectorDriver, VectorItem, VectorMatch};
use crate::FrameworkError;
use async_trait::async_trait;
use qdrant_client::qdrant::{
    CountPointsBuilder, CreateCollectionBuilder, DeletePointsBuilder, Distance, PointId,
    PointStruct, PointsIdsList, QueryPointsBuilder, ScoredPoint, UpsertPointsBuilder,
    VectorParamsBuilder, point_id::PointIdOptions, points_selector::PointsSelectorOneOf,
};
use qdrant_client::{Payload, Qdrant, QdrantError};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Reserved payload key. The framework writes the caller's original
/// `VectorItem::id` here on upsert so similarity hits can map back to
/// the same string even when the Qdrant-side `PointId` is a derived
/// UUID-5. Stripped from `VectorMatch::metadata` on retrieval.
pub const SUPRNOVA_ID_PAYLOAD_KEY: &str = "__suprnova_id";

/// Stable namespace UUID used to derive deterministic v5 IDs from
/// arbitrary user-supplied strings. Versions of Suprnova MUST NOT
/// change this — derived point IDs would shift, orphaning data
/// written by older versions.
const SUPRNOVA_VECTOR_NAMESPACE: Uuid =
    Uuid::from_u128(0xab8e_7d4a_5f9b_4034_a8e7_72f6_a8b3_c0d9_u128);

/// Distance metric for auto-created Qdrant collections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QdrantDistance {
    /// Cosine similarity — default; matches the in-process Memory driver.
    #[default]
    Cosine,
    /// Euclidean (L2) distance.
    Euclidean,
    /// Dot product. Vectors must be normalized for this to behave as a
    /// similarity score.
    Dot,
    /// Manhattan (L1) distance.
    Manhattan,
}

impl From<QdrantDistance> for Distance {
    fn from(d: QdrantDistance) -> Self {
        match d {
            QdrantDistance::Cosine => Distance::Cosine,
            QdrantDistance::Euclidean => Distance::Euclid,
            QdrantDistance::Dot => Distance::Dot,
            QdrantDistance::Manhattan => Distance::Manhattan,
        }
    }
}

/// Qdrant-backed [`VectorDriver`].
pub struct QdrantVectorDriver {
    client: Arc<Qdrant>,
    auto_create: bool,
    distance: Distance,
    known_collections: Arc<RwLock<HashSet<String>>>,
}

impl QdrantVectorDriver {
    /// Wrap an already-built `qdrant_client::Qdrant`. Use this when
    /// you need TLS, custom timeouts, or anything else only the raw
    /// `Qdrant::from_url(...)` builder exposes.
    pub fn from_client(client: Qdrant) -> Self {
        Self {
            client: Arc::new(client),
            auto_create: true,
            distance: Distance::Cosine,
            known_collections: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Construct against an unauthenticated Qdrant at `url`. `url` is
    /// a gRPC URL — by default port 6334.
    pub fn from_url(url: &str) -> Result<Self, FrameworkError> {
        let client = Qdrant::from_url(url)
            .build()
            .map_err(|e| FrameworkError::internal(format!("qdrant client init at '{url}': {e}")))?;
        Ok(Self::from_client(client))
    }

    /// Construct against an API-key-gated Qdrant (Qdrant Cloud, or
    /// any self-hosted instance with an API key configured).
    pub fn from_url_with_api_key(
        url: &str,
        api_key: impl Into<String>,
    ) -> Result<Self, FrameworkError> {
        let client = Qdrant::from_url(url)
            .api_key(api_key.into())
            .build()
            .map_err(|e| FrameworkError::internal(format!("qdrant client init at '{url}': {e}")))?;
        Ok(Self::from_client(client))
    }

    /// When `false`, the driver requires the collection to exist
    /// before any upsert (returns `not_found` otherwise). Default: `true`.
    pub fn with_auto_create(mut self, on: bool) -> Self {
        self.auto_create = on;
        self
    }

    /// Set the distance metric used when auto-creating collections.
    /// Has no effect on collections that already exist. Default: Cosine.
    pub fn with_distance(mut self, d: QdrantDistance) -> Self {
        self.distance = d.into();
        self
    }

    /// Borrow the underlying `qdrant_client::Qdrant`. Use this when
    /// the trait surface isn't enough — filter expressions on
    /// search, scroll, snapshots, quantization knobs.
    pub fn client(&self) -> &Qdrant {
        &self.client
    }

    /// Compute the `PointId` the driver writes for a given caller-side
    /// id. Exposed so direct `qdrant_client::Qdrant` calls target the
    /// same points the framework writes.
    pub fn resolve_point_id(id: &str) -> PointId {
        if let Ok(n) = id.parse::<u64>() {
            return PointId::from(n);
        }
        if Uuid::parse_str(id).is_ok() {
            return PointId::from(id.to_string());
        }
        let derived = Uuid::new_v5(&SUPRNOVA_VECTOR_NAMESPACE, id.as_bytes());
        // `From<Uuid> for PointId` is gated behind `qdrant-client`'s
        // `uuid` feature; routing through a String hits the always-on
        // `From<String>` impl, which wraps the value in the UUID
        // variant. `derived.to_string()` is guaranteed-valid UUID.
        PointId::from(derived.to_string())
    }

    /// Encode a [`VectorItem`] into a Qdrant `PointStruct`, stashing
    /// the caller-side id under [`SUPRNOVA_ID_PAYLOAD_KEY`] in the
    /// payload. Returns a `param` error if `metadata` is neither a
    /// JSON object nor null.
    pub fn build_point(item: VectorItem) -> Result<PointStruct, FrameworkError> {
        let point_id = Self::resolve_point_id(&item.id);
        let mut payload_obj = match item.metadata {
            serde_json::Value::Object(map) => map,
            serde_json::Value::Null => serde_json::Map::new(),
            other => {
                return Err(FrameworkError::param(format!(
                    "vector item '{}' metadata must be a JSON object or null, got: {}",
                    item.id, other
                )));
            }
        };
        payload_obj.insert(
            SUPRNOVA_ID_PAYLOAD_KEY.to_string(),
            serde_json::Value::String(item.id.clone()),
        );
        let payload: Payload =
            Payload::try_from(serde_json::Value::Object(payload_obj)).map_err(|e| {
                FrameworkError::internal(format!("qdrant payload encode for '{}': {e}", item.id))
            })?;
        Ok(PointStruct::new(point_id, item.embedding, payload))
    }

    /// Decode a Qdrant `ScoredPoint` into a framework-side
    /// [`VectorMatch`], reading the caller-side id from
    /// [`SUPRNOVA_ID_PAYLOAD_KEY`] and stripping it from the returned
    /// metadata.
    pub fn decode_match(sp: ScoredPoint) -> VectorMatch {
        let payload_json = serde_json::Value::from(Payload::from(sp.payload));
        let id = Self::recover_id(sp.id.as_ref(), &payload_json);
        let metadata = match payload_json {
            serde_json::Value::Object(mut map) => {
                map.remove(SUPRNOVA_ID_PAYLOAD_KEY);
                serde_json::Value::Object(map)
            }
            other => other,
        };
        VectorMatch {
            id,
            score: sp.score,
            metadata,
        }
    }

    fn recover_id(point_id: Option<&PointId>, payload: &serde_json::Value) -> String {
        if let Some(val) = payload
            .get(SUPRNOVA_ID_PAYLOAD_KEY)
            .and_then(|v| v.as_str())
        {
            return val.to_string();
        }
        match point_id.and_then(|p| p.point_id_options.as_ref()) {
            Some(PointIdOptions::Num(n)) => n.to_string(),
            Some(PointIdOptions::Uuid(s)) => s.clone(),
            None => String::new(),
        }
    }

    async fn ensure_collection(&self, name: &str, dim: usize) -> Result<(), FrameworkError> {
        {
            let guard = self.known_collections.read().await;
            if guard.contains(name) {
                return Ok(());
            }
        }
        let exists = self
            .client
            .collection_exists(name.to_string())
            .await
            .map_err(|e| {
                FrameworkError::internal(format!("qdrant collection_exists '{name}': {e}"))
            })?;
        if !exists {
            if !self.auto_create {
                return Err(FrameworkError::not_found(format!(
                    "qdrant collection '{name}' does not exist (auto_create=false)"
                )));
            }
            let result = self
                .client
                .create_collection(
                    CreateCollectionBuilder::new(name)
                        .vectors_config(VectorParamsBuilder::new(dim as u64, self.distance)),
                )
                .await;
            if let Err(e) = result {
                // Race: a concurrent caller may have created it.
                // Treat re-check-as-exists as success and continue.
                let exists_now = self
                    .client
                    .collection_exists(name.to_string())
                    .await
                    .map_err(|inner| {
                        FrameworkError::internal(format!(
                            "qdrant collection_exists '{name}': {inner}"
                        ))
                    })?;
                if !exists_now {
                    return Err(FrameworkError::internal(format!(
                        "qdrant create_collection '{name}': {e}"
                    )));
                }
            }
        }
        self.known_collections
            .write()
            .await
            .insert(name.to_string());
        Ok(())
    }

    // qdrant-client 1.18 does not surface a typed `CollectionNotFound`
    // variant on `QdrantError` — server-side missing-collection errors
    // come back as `ResponseError { status }` where the inner gRPC
    // status carries the meaningful detail. We fall back to a string
    // heuristic over the full error display. If a future qdrant-client
    // release adds a typed variant or a stable `code()` accessor, swap
    // this for a structural match.
    fn looks_like_collection_missing(err: &QdrantError) -> bool {
        let s = err.to_string().to_lowercase();
        s.contains("doesn't exist") || s.contains("not found") || s.contains("not exist")
    }
}

#[async_trait]
impl VectorDriver for QdrantVectorDriver {
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
        let points: Vec<PointStruct> = items
            .into_iter()
            .map(Self::build_point)
            .collect::<Result<_, _>>()?;

        self.ensure_collection(store, dim).await?;
        match self
            .client
            .upsert_points(UpsertPointsBuilder::new(store, points.clone()).wait(true))
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if Self::looks_like_collection_missing(&e) => {
                // Cache claimed the collection existed but the server
                // disagrees (dropped externally, or Qdrant restarted
                // before persistence flushed). Drop the cache entry,
                // ensure again, and retry once.
                self.known_collections.write().await.remove(store);
                self.ensure_collection(store, dim).await?;
                self.client
                    .upsert_points(UpsertPointsBuilder::new(store, points).wait(true))
                    .await
                    .map_err(|inner| {
                        FrameworkError::internal(format!(
                            "qdrant upsert into '{store}' (after cache invalidate): {inner}"
                        ))
                    })?;
                Ok(())
            }
            Err(e) => Err(FrameworkError::internal(format!(
                "qdrant upsert into '{store}': {e}"
            ))),
        }
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
            return Err(FrameworkError::param(
                "vector::similar query is zero-vector",
            ));
        }

        match self
            .client
            .query(
                QueryPointsBuilder::new(store)
                    .query(query)
                    .limit(k as u64)
                    .with_payload(true),
            )
            .await
        {
            Ok(response) => Ok(response
                .result
                .into_iter()
                .map(Self::decode_match)
                .collect()),
            Err(e) if Self::looks_like_collection_missing(&e) => {
                // Stale cache: ensure_collection had marked this store
                // as known, but the server says it's gone (dropped
                // externally, restart-without-persistence). Drop the
                // cache entry so the next ensure_collection re-creates
                // the collection on demand; a missing collection has
                // no matches, so the read-path answer is "no results."
                self.known_collections.write().await.remove(store);
                Ok(Vec::new())
            }
            Err(e) => Err(FrameworkError::internal(format!(
                "qdrant query on '{store}': {e}"
            ))),
        }
    }

    async fn delete(&self, store: &str, ids: Vec<String>) -> Result<(), FrameworkError> {
        if ids.is_empty() {
            return Ok(());
        }
        let point_ids: Vec<PointId> = ids.iter().map(|id| Self::resolve_point_id(id)).collect();
        match self
            .client
            .delete_points(
                DeletePointsBuilder::new(store)
                    .points(PointsSelectorOneOf::Points(PointsIdsList {
                        ids: point_ids,
                    }))
                    .wait(true),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if Self::looks_like_collection_missing(&e) => {
                // Same recovery as similar(): a missing collection
                // means there's nothing to delete. Drop the cache
                // entry so subsequent writes recreate the collection
                // via ensure_collection.
                self.known_collections.write().await.remove(store);
                Ok(())
            }
            Err(e) => Err(FrameworkError::internal(format!(
                "qdrant delete from '{store}': {e}"
            ))),
        }
    }

    async fn count(&self, store: &str) -> Result<usize, FrameworkError> {
        match self
            .client
            .count(CountPointsBuilder::new(store).exact(true))
            .await
        {
            Ok(response) => Ok(response.result.map(|r| r.count as usize).unwrap_or(0)),
            Err(e) if Self::looks_like_collection_missing(&e) => {
                // A missing collection means 0 points. Invalidate the
                // cache so the next ensure_collection writes the
                // collection back rather than blindly trusting a stale
                // entry that points at a server-side absence.
                self.known_collections.write().await.remove(store);
                Ok(0)
            }
            Err(e) => Err(FrameworkError::internal(format!(
                "qdrant count on '{store}': {e}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(msg: &str) -> QdrantError {
        // QdrantError::ConversionError lets us wrap an arbitrary string
        // without dragging tonic into dev-deps. The recovery helper
        // matches on the lowercased Display body, which works the same
        // shape regardless of which variant produced the text.
        QdrantError::ConversionError(msg.to_string())
    }

    /// All three read/delete recovery paths route through
    /// `looks_like_collection_missing`. Pin its substring set so a
    /// silent variant rename in qdrant-client (or a server-side message
    /// rewording) doesn't quietly cause every recovery arm to stop
    /// firing — which would re-introduce the L32 leak of stale cache
    /// entries on read paths.
    #[test]
    fn looks_like_collection_missing_matches_known_messages() {
        // Qdrant historically returned each of these phrasings; new
        // releases occasionally swap between them. Match on all three
        // case-insensitively.
        assert!(QdrantVectorDriver::looks_like_collection_missing(&err(
            "Collection `users` doesn't exist!"
        )));
        assert!(QdrantVectorDriver::looks_like_collection_missing(&err(
            "Collection users does not exist"
        )));
        assert!(QdrantVectorDriver::looks_like_collection_missing(&err(
            "Not found: collection users"
        )));
        // Should NOT match unrelated errors — e.g. dimension mismatch
        // is a different recovery class entirely.
        assert!(!QdrantVectorDriver::looks_like_collection_missing(&err(
            "Wrong vector size: expected 768, got 1536"
        )));
        assert!(!QdrantVectorDriver::looks_like_collection_missing(&err(
            "Internal server error"
        )));
    }
}
