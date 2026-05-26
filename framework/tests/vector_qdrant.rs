//! Phase 9A — Qdrant vector driver tests.
//!
//! Two layers:
//!
//! 1. **Pure-function tests** (no `#[ignore]`) — always run; cover id
//!    resolution, payload encode/decode round-trips, and the trait
//!    surface's short-circuit paths (empty inputs, `k = 0`). These
//!    don't touch the network — the driver short-circuits before any
//!    RPC call.
//!
//! 2. **Integration tests** (`#[ignore]`) — exercise the driver
//!    against a real Qdrant server. Skipped by default; run via
//!
//!    ```bash
//!    docker run -p 6334:6334 -p 6333:6333 qdrant/qdrant
//!    QDRANT_URL=http://localhost:6334 \
//!        cargo test -p suprnova --test vector_qdrant -- --ignored
//!    ```
//!
//!    Each test uses a unique collection name (timestamp-tagged) so
//!    parallel runs against the same server don't clash, and tears
//!    its collection down on the way out via the underlying
//!    [`qdrant_client::Qdrant::delete_collection`] surface.

use std::collections::HashMap;
use suprnova::vector::driver::VectorDriver;
use suprnova::{QdrantDistance, QdrantVectorDriver, SUPRNOVA_ID_PAYLOAD_KEY, VectorItem};

use qdrant_client::Payload;
use qdrant_client::qdrant::{PointId, ScoredPoint, Value as QdrantValue, point_id::PointIdOptions};

fn unreachable_driver() -> QdrantVectorDriver {
    QdrantVectorDriver::from_url("http://127.0.0.1:1").expect("driver builds without connecting")
}

fn point_id_opts(pid: PointId) -> PointIdOptions {
    pid.point_id_options
        .expect("PointId carries an option variant")
}

fn payload_hm(json: serde_json::Value) -> HashMap<String, QdrantValue> {
    Payload::try_from(json)
        .expect("must be a JSON object")
        .into()
}

fn unique_collection(tag: &str) -> String {
    format!(
        "p9a_t2_{tag}_{}",
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    )
}

fn qdrant_url_or_skip(test_name: &str) -> Option<String> {
    match std::env::var("QDRANT_URL") {
        Ok(url) => Some(url),
        Err(_) => {
            eprintln!("[{test_name}] skipping: QDRANT_URL not set");
            None
        }
    }
}

async fn drop_collection(driver: &QdrantVectorDriver, collection: &str) {
    let _ = driver
        .client()
        .delete_collection(collection.to_string())
        .await;
}

// ---------------------------------------------------------------------
// Pure-function tests — id resolution
// ---------------------------------------------------------------------

#[test]
fn resolve_point_id_for_integer_string_returns_num_variant() {
    let pid = QdrantVectorDriver::resolve_point_id("42");
    match point_id_opts(pid) {
        PointIdOptions::Num(n) => assert_eq!(n, 42),
        other => panic!("expected Num(42), got {other:?}"),
    }
}

#[test]
fn resolve_point_id_for_uuid_string_returns_uuid_verbatim() {
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let pid = QdrantVectorDriver::resolve_point_id(uuid);
    match point_id_opts(pid) {
        PointIdOptions::Uuid(s) => assert_eq!(s, uuid, "uuid stored as-is"),
        other => panic!("expected Uuid, got {other:?}"),
    }
}

#[test]
fn resolve_point_id_for_arbitrary_string_returns_derived_uuid() {
    let pid = QdrantVectorDriver::resolve_point_id("hello-world");
    match point_id_opts(pid) {
        PointIdOptions::Uuid(s) => {
            uuid::Uuid::parse_str(&s).expect("derived value must be a valid UUID");
        }
        other => panic!("expected derived Uuid, got {other:?}"),
    }
}

#[test]
fn resolve_point_id_is_deterministic() {
    let a = QdrantVectorDriver::resolve_point_id("docs::v1::page-42");
    let b = QdrantVectorDriver::resolve_point_id("docs::v1::page-42");
    assert_eq!(point_id_opts(a), point_id_opts(b));
}

#[test]
fn resolve_point_id_distinct_strings_get_distinct_ids() {
    let a = QdrantVectorDriver::resolve_point_id("alpha");
    let b = QdrantVectorDriver::resolve_point_id("beta");
    assert_ne!(point_id_opts(a), point_id_opts(b));
}

// ---------------------------------------------------------------------
// Pure-function tests — payload encode (build_point)
// ---------------------------------------------------------------------

#[test]
fn build_point_includes_suprnova_id_in_payload() {
    let point = QdrantVectorDriver::build_point(VectorItem::new(
        "doc-42",
        vec![1.0, 0.0],
        serde_json::json!({ "title": "Hello" }),
    ))
    .unwrap();
    let payload_json = serde_json::Value::from(Payload::from(point.payload));
    assert_eq!(
        payload_json[SUPRNOVA_ID_PAYLOAD_KEY],
        serde_json::Value::String("doc-42".to_string())
    );
    assert_eq!(
        payload_json["title"],
        serde_json::Value::String("Hello".into())
    );
}

#[test]
fn build_point_accepts_null_metadata() {
    let point = QdrantVectorDriver::build_point(VectorItem::new(
        "doc-1",
        vec![0.5, 0.5],
        serde_json::Value::Null,
    ))
    .unwrap();
    let payload_json = serde_json::Value::from(Payload::from(point.payload));
    assert_eq!(payload_json[SUPRNOVA_ID_PAYLOAD_KEY], "doc-1");
}

#[test]
fn build_point_rejects_non_object_metadata() {
    let err = QdrantVectorDriver::build_point(VectorItem::new(
        "doc-1",
        vec![0.5, 0.5],
        serde_json::json!("a bare string is not an object"),
    ))
    .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("doc-1"), "error names the offending id: {msg}");
    assert!(
        msg.contains("JSON object"),
        "error names the constraint: {msg}"
    );
}

#[test]
fn build_point_overrides_caller_supplied_reserved_key() {
    let point = QdrantVectorDriver::build_point(VectorItem::new(
        "real-id",
        vec![1.0],
        serde_json::json!({ SUPRNOVA_ID_PAYLOAD_KEY: "spoofed-id" }),
    ))
    .unwrap();
    let payload_json = serde_json::Value::from(Payload::from(point.payload));
    assert_eq!(payload_json[SUPRNOVA_ID_PAYLOAD_KEY], "real-id");
}

// ---------------------------------------------------------------------
// Pure-function tests — match decode (decode_match)
// ---------------------------------------------------------------------

#[test]
fn decode_match_strips_suprnova_id_from_metadata() {
    let sp = ScoredPoint {
        id: Some(PointId::from(99u64)),
        payload: payload_hm(serde_json::json!({
            SUPRNOVA_ID_PAYLOAD_KEY: "my-original-id",
            "title": "Hello",
        })),
        score: 0.93,
        ..Default::default()
    };
    let m = QdrantVectorDriver::decode_match(sp);
    assert_eq!(m.id, "my-original-id");
    assert!((m.score - 0.93).abs() < 1e-6);
    assert!(
        m.metadata.get(SUPRNOVA_ID_PAYLOAD_KEY).is_none(),
        "reserved key must be stripped, got: {}",
        m.metadata
    );
    assert_eq!(m.metadata["title"], "Hello");
}

#[test]
fn decode_match_recovers_id_from_payload_even_if_pointid_differs() {
    let sp = ScoredPoint {
        id: Some(PointId::from(
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, b"example").to_string(),
        )),
        payload: payload_hm(serde_json::json!({
            SUPRNOVA_ID_PAYLOAD_KEY: "user-facing-id",
        })),
        score: 1.0,
        ..Default::default()
    };
    let m = QdrantVectorDriver::decode_match(sp);
    assert_eq!(m.id, "user-facing-id");
}

#[test]
fn decode_match_falls_back_to_pointid_when_no_payload_key() {
    let sp = ScoredPoint {
        id: Some(PointId::from(7u64)),
        payload: HashMap::new(),
        score: 0.5,
        ..Default::default()
    };
    let m = QdrantVectorDriver::decode_match(sp);
    assert_eq!(m.id, "7", "Num variant falls back to its string form");
}

#[test]
fn decode_match_falls_back_to_uuid_variant_when_no_payload_key() {
    let sp = ScoredPoint {
        id: Some(PointId::from(
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
        )),
        payload: HashMap::new(),
        score: 0.5,
        ..Default::default()
    };
    let m = QdrantVectorDriver::decode_match(sp);
    assert_eq!(m.id, "550e8400-e29b-41d4-a716-446655440000");
}

// ---------------------------------------------------------------------
// Pure-function tests — short-circuits (no real network)
// ---------------------------------------------------------------------

#[tokio::test]
async fn upsert_with_empty_items_is_no_op() {
    let driver = unreachable_driver();
    driver.upsert("never-reached", vec![]).await.unwrap();
}

#[tokio::test]
async fn delete_with_empty_ids_is_no_op() {
    let driver = unreachable_driver();
    driver
        .delete("never-reached", Vec::<String>::new())
        .await
        .unwrap();
}

#[tokio::test]
async fn similar_with_k_zero_returns_empty_without_call() {
    let driver = unreachable_driver();
    let hits = driver
        .similar("never-reached", vec![1.0, 0.0], 0)
        .await
        .unwrap();
    assert!(hits.is_empty());
}

#[tokio::test]
async fn similar_with_empty_query_errors_locally() {
    let driver = unreachable_driver();
    let err = driver
        .similar("never-reached", Vec::<f32>::new(), 5)
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("empty"),
        "error names the cause: {err}"
    );
}

#[tokio::test]
async fn similar_with_zero_vector_errors_locally() {
    let driver = unreachable_driver();
    let err = driver
        .similar("never-reached", vec![0.0, 0.0, 0.0], 5)
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("zero-vector"),
        "error names the cause: {err}"
    );
}

#[tokio::test]
async fn upsert_with_zero_dim_first_item_errors_locally() {
    let driver = unreachable_driver();
    let err = driver
        .upsert(
            "never-reached",
            vec![VectorItem::new(
                "a",
                Vec::<f32>::new(),
                serde_json::json!({}),
            )],
        )
        .await
        .unwrap_err();
    assert!(
        format!("{err}").contains("zero-length"),
        "error names the cause: {err}"
    );
}

// ---------------------------------------------------------------------
// Pure-function tests — config / constants
// ---------------------------------------------------------------------

#[test]
fn distance_default_is_cosine() {
    assert_eq!(QdrantDistance::default(), QdrantDistance::Cosine);
}

#[test]
fn suprnova_id_payload_key_uses_reserved_double_underscore_prefix() {
    assert!(
        SUPRNOVA_ID_PAYLOAD_KEY.starts_with("__"),
        "the reserved key must use the framework's __-prefix convention"
    );
}

// ---------------------------------------------------------------------
// Integration tests — env-gated, require a running Qdrant
// ---------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a running Qdrant at QDRANT_URL"]
async fn integration_upsert_and_count_roundtrip() {
    let Some(url) = qdrant_url_or_skip("upsert_and_count_roundtrip") else {
        return;
    };
    let driver = QdrantVectorDriver::from_url(&url).unwrap();
    let coll = unique_collection("count");

    driver
        .upsert(
            &coll,
            vec![
                VectorItem::new("a", vec![1.0, 0.0, 0.0], serde_json::json!({"tag": 1})),
                VectorItem::new("b", vec![0.0, 1.0, 0.0], serde_json::json!({"tag": 2})),
            ],
        )
        .await
        .unwrap();
    assert_eq!(driver.count(&coll).await.unwrap(), 2);

    drop_collection(&driver, &coll).await;
}

#[tokio::test]
#[ignore = "requires a running Qdrant at QDRANT_URL"]
async fn integration_upsert_same_id_replaces_existing_point() {
    let Some(url) = qdrant_url_or_skip("upsert_same_id_replaces") else {
        return;
    };
    let driver = QdrantVectorDriver::from_url(&url).unwrap();
    let coll = unique_collection("replace");

    driver
        .upsert(
            &coll,
            vec![VectorItem::new(
                "x",
                vec![1.0, 0.0],
                serde_json::json!({"v": 1}),
            )],
        )
        .await
        .unwrap();
    driver
        .upsert(
            &coll,
            vec![VectorItem::new(
                "x",
                vec![0.0, 1.0],
                serde_json::json!({"v": 2}),
            )],
        )
        .await
        .unwrap();
    assert_eq!(driver.count(&coll).await.unwrap(), 1, "same id merges");
    let hits = driver.similar(&coll, vec![0.0, 1.0], 1).await.unwrap();
    assert_eq!(hits[0].id, "x");
    assert_eq!(
        hits[0].metadata["v"],
        serde_json::json!(2),
        "metadata follows the new embedding"
    );

    drop_collection(&driver, &coll).await;
}

#[tokio::test]
#[ignore = "requires a running Qdrant at QDRANT_URL"]
async fn integration_similar_returns_top_k_descending() {
    let Some(url) = qdrant_url_or_skip("similar_top_k") else {
        return;
    };
    let driver = QdrantVectorDriver::from_url(&url).unwrap();
    let coll = unique_collection("topk");

    driver
        .upsert(
            &coll,
            vec![
                VectorItem::new("perfect", vec![1.0, 0.0, 0.0], serde_json::json!({})),
                VectorItem::new("orthogonal", vec![0.0, 1.0, 0.0], serde_json::json!({})),
                VectorItem::new("close", vec![0.9, 0.1, 0.0], serde_json::json!({})),
            ],
        )
        .await
        .unwrap();
    let hits = driver.similar(&coll, vec![1.0, 0.0, 0.0], 3).await.unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].id, "perfect");
    assert_eq!(hits[1].id, "close");
    assert_eq!(hits[2].id, "orthogonal");
    assert!(hits[0].score >= hits[1].score);
    assert!(hits[1].score >= hits[2].score);

    drop_collection(&driver, &coll).await;
}

#[tokio::test]
#[ignore = "requires a running Qdrant at QDRANT_URL"]
async fn integration_delete_removes_points_by_id() {
    let Some(url) = qdrant_url_or_skip("delete_by_id") else {
        return;
    };
    let driver = QdrantVectorDriver::from_url(&url).unwrap();
    let coll = unique_collection("del");

    driver
        .upsert(
            &coll,
            vec![
                VectorItem::new("keep", vec![1.0, 0.0], serde_json::json!({})),
                VectorItem::new("toss", vec![0.0, 1.0], serde_json::json!({})),
            ],
        )
        .await
        .unwrap();
    driver
        .delete(&coll, vec!["toss".to_string()])
        .await
        .unwrap();
    assert_eq!(driver.count(&coll).await.unwrap(), 1);
    let hits = driver.similar(&coll, vec![0.0, 1.0], 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "keep");

    drop_collection(&driver, &coll).await;
}

#[tokio::test]
#[ignore = "requires a running Qdrant at QDRANT_URL"]
async fn integration_metadata_strips_suprnova_id_on_retrieval() {
    let Some(url) = qdrant_url_or_skip("strip_metadata") else {
        return;
    };
    let driver = QdrantVectorDriver::from_url(&url).unwrap();
    let coll = unique_collection("strip");

    driver
        .upsert(
            &coll,
            vec![VectorItem::new(
                "arbitrary-string-id",
                vec![1.0, 0.0],
                serde_json::json!({"tag": "v1"}),
            )],
        )
        .await
        .unwrap();
    let hits = driver.similar(&coll, vec![1.0, 0.0], 1).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].id, "arbitrary-string-id",
        "caller-side id round-trips"
    );
    assert!(
        hits[0].metadata.get(SUPRNOVA_ID_PAYLOAD_KEY).is_none(),
        "reserved key MUST NOT leak into caller-side metadata"
    );
    assert_eq!(hits[0].metadata["tag"], "v1");

    drop_collection(&driver, &coll).await;
}

#[tokio::test]
#[ignore = "requires a running Qdrant at QDRANT_URL"]
async fn integration_auto_create_collection_from_cold() {
    let Some(url) = qdrant_url_or_skip("auto_create") else {
        return;
    };
    let driver = QdrantVectorDriver::from_url(&url).unwrap();
    let coll = unique_collection("auto");

    driver
        .upsert(
            &coll,
            vec![VectorItem::new(
                "a",
                vec![1.0, 0.0, 0.0, 0.0],
                serde_json::json!({}),
            )],
        )
        .await
        .unwrap();
    assert_eq!(driver.count(&coll).await.unwrap(), 1);

    drop_collection(&driver, &coll).await;
}

#[tokio::test]
#[ignore = "requires a running Qdrant at QDRANT_URL"]
async fn integration_auto_create_disabled_yields_not_found() {
    let Some(url) = qdrant_url_or_skip("auto_create_off") else {
        return;
    };
    let driver = QdrantVectorDriver::from_url(&url)
        .unwrap()
        .with_auto_create(false);
    let coll = unique_collection("noauto");

    let err = driver
        .upsert(
            &coll,
            vec![VectorItem::new("a", vec![1.0, 0.0], serde_json::json!({}))],
        )
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("does not exist"),
        "error names the missing collection: {msg}"
    );
}
