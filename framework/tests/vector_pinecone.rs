//! Phase 9A — Pinecone vector driver tests.
//!
//! Requires `--features vector-pinecone` because the Pinecone driver
//! is feature-gated (production gate #370 — pinecone-sdk pulls four
//! active rustls-webpki CVEs through tonic 0.11.0).
//!
//! Two layers (same pattern as `vector_qdrant.rs`):

#![cfg(feature = "vector-pinecone")]

//!
//! 1. **Pure-function tests** (always run) — JSON ↔ protobuf
//!    metadata round-trips, field decode, the trait's short-circuit
//!    paths (empty inputs, k = 0, empty/zero-vector queries). None
//!    touches the network.
//!
//! 2. **Integration tests** (`#[ignore]`) — drive a real Pinecone
//!    account. Require both:
//!    - `PINECONE_API_KEY` — your account's API key
//!    - `PINECONE_TEST_INDEX` — a pre-existing serverless index name
//!      (the driver doesn't auto-create indexes; see the module docs).
//!
//!    Each integration test uses a unique namespace
//!    (timestamp-tagged) and cleans it up with `delete_all` on the
//!    way out — your test index is reused but never polluted.
//!
//!    ```bash
//!    PINECONE_API_KEY=... PINECONE_TEST_INDEX=my-test-index \
//!        cargo test -p suprnova --test vector_pinecone -- --ignored
//!    ```

use suprnova::vector::driver::VectorDriver;
use suprnova::{PineconeVectorDriver, VectorItem};

use pinecone_sdk::models::Namespace;
use prost_types::{Struct as PbStruct, Value as PbValue, value::Kind};

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------

fn unreachable_driver() -> PineconeVectorDriver {
    // Any string is a valid api key for the constructor — actual
    // requests would fail-401 but we never make any in pure-fn tests
    // (the driver short-circuits before any RPC).
    PineconeVectorDriver::from_api_key("test-key-only-used-locally")
        .expect("driver builds without network")
}

fn unique_namespace(tag: &str) -> String {
    format!(
        "p9a_t3_{tag}_{}",
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    )
}

fn pinecone_env_or_skip(test_name: &str) -> Option<(String, String)> {
    let api_key = std::env::var("PINECONE_API_KEY").ok();
    let index = std::env::var("PINECONE_TEST_INDEX").ok();
    match (api_key, index) {
        (Some(a), Some(i)) => Some((a, i)),
        _ => {
            eprintln!(
                "[{test_name}] skipping: set PINECONE_API_KEY and PINECONE_TEST_INDEX to run"
            );
            None
        }
    }
}

async fn delete_namespace(driver: &PineconeVectorDriver, index_name: &str, namespace: &str) {
    let Ok(description) = driver.client().describe_index(index_name).await else {
        return;
    };
    let Ok(mut index) = driver.client().index(&description.host).await else {
        return;
    };
    let _ = index
        .delete_all(&Namespace {
            name: namespace.to_string(),
        })
        .await;
}

// ----------------------------------------------------------------------
// Pure-function tests — metadata conversion (json ↔ protobuf)
// ----------------------------------------------------------------------

#[test]
fn json_to_metadata_null_yields_none() {
    let got = PineconeVectorDriver::json_to_metadata(serde_json::Value::Null).unwrap();
    assert!(got.is_none());
}

#[test]
fn json_to_metadata_empty_object_yields_empty_struct() {
    let got = PineconeVectorDriver::json_to_metadata(serde_json::json!({})).unwrap();
    let s = got.expect("empty object is still Some");
    assert!(s.fields.is_empty());
}

#[test]
fn json_to_metadata_rejects_non_object_non_null() {
    let err =
        PineconeVectorDriver::json_to_metadata(serde_json::json!("a bare string")).unwrap_err();
    assert!(
        format!("{err}").contains("JSON object or null"),
        "error names the constraint: {err}"
    );
}

#[test]
fn json_to_metadata_round_trip_preserves_all_primitive_kinds() {
    let original = serde_json::json!({
        "string_field": "hello",
        "bool_field": true,
        "number_field": 42.5,
        "null_field": null,
        "array_field": [1, "two", false],
        "nested": { "inner": "value" }
    });
    let pb = PineconeVectorDriver::json_to_metadata(original.clone())
        .unwrap()
        .expect("object yields Some");
    let back = PineconeVectorDriver::metadata_to_json(Some(pb));
    assert_eq!(back["string_field"], "hello");
    assert_eq!(back["bool_field"], true);
    assert_eq!(back["number_field"], 42.5);
    assert_eq!(back["null_field"], serde_json::Value::Null);
    assert_eq!(back["array_field"][0], 1.0);
    assert_eq!(back["array_field"][1], "two");
    assert_eq!(back["array_field"][2], false);
    assert_eq!(back["nested"]["inner"], "value");
}

#[test]
fn metadata_to_json_none_yields_null() {
    let got = PineconeVectorDriver::metadata_to_json(None);
    assert_eq!(got, serde_json::Value::Null);
}

#[test]
fn metadata_to_json_empty_struct_yields_empty_object() {
    let got = PineconeVectorDriver::metadata_to_json(Some(PbStruct {
        fields: Default::default(),
    }));
    assert_eq!(got, serde_json::json!({}));
}

#[test]
fn metadata_to_json_unknown_pb_kind_yields_null() {
    // PbValue with kind=None happens when a field is wire-present
    // but its tag wasn't recognized. We map it to JSON null to be
    // forgiving.
    let mut fields = std::collections::BTreeMap::new();
    fields.insert("mystery".to_string(), PbValue { kind: None });
    let got = PineconeVectorDriver::metadata_to_json(Some(PbStruct { fields }));
    assert_eq!(got["mystery"], serde_json::Value::Null);
}

#[test]
fn metadata_to_json_round_trip_preserves_pb_kinds() {
    let mut fields = std::collections::BTreeMap::new();
    fields.insert(
        "a_bool".to_string(),
        PbValue {
            kind: Some(Kind::BoolValue(true)),
        },
    );
    fields.insert(
        "a_number".to_string(),
        PbValue {
            kind: Some(Kind::NumberValue(4.25)),
        },
    );
    fields.insert(
        "a_string".to_string(),
        PbValue {
            kind: Some(Kind::StringValue("hi".into())),
        },
    );
    fields.insert(
        "a_null".to_string(),
        PbValue {
            kind: Some(Kind::NullValue(0)),
        },
    );
    let pb = PbStruct { fields };
    let json = PineconeVectorDriver::metadata_to_json(Some(pb));
    assert_eq!(json["a_bool"], true);
    assert_eq!(json["a_number"], 4.25);
    assert_eq!(json["a_string"], "hi");
    assert_eq!(json["a_null"], serde_json::Value::Null);
}

// ----------------------------------------------------------------------
// Pure-function tests — vector encode (build_vector)
// ----------------------------------------------------------------------

#[test]
fn build_vector_passes_id_through_unchanged() {
    let v = PineconeVectorDriver::build_vector(VectorItem::new(
        "anything-goes-as-an-id-✓",
        vec![1.0, 0.0],
        serde_json::json!({}),
    ))
    .unwrap();
    assert_eq!(v.id, "anything-goes-as-an-id-✓");
}

#[test]
fn build_vector_includes_embedding_values() {
    let v = PineconeVectorDriver::build_vector(VectorItem::new(
        "id",
        vec![1.0, 2.0, 3.0],
        serde_json::json!({}),
    ))
    .unwrap();
    assert_eq!(v.values, vec![1.0, 2.0, 3.0]);
}

#[test]
fn build_vector_attaches_metadata_when_object() {
    let v = PineconeVectorDriver::build_vector(VectorItem::new(
        "id",
        vec![1.0],
        serde_json::json!({"tag": "important"}),
    ))
    .unwrap();
    let metadata = v.metadata.expect("object metadata is Some");
    assert!(metadata.fields.contains_key("tag"));
}

#[test]
fn build_vector_with_null_metadata_yields_none() {
    let v = PineconeVectorDriver::build_vector(VectorItem::new(
        "id",
        vec![1.0],
        serde_json::Value::Null,
    ))
    .unwrap();
    assert!(v.metadata.is_none());
}

#[test]
fn build_vector_rejects_non_object_metadata() {
    let err = PineconeVectorDriver::build_vector(VectorItem::new(
        "id",
        vec![1.0],
        serde_json::json!("not an object"),
    ))
    .unwrap_err();
    assert!(format!("{err}").contains("JSON object or null"));
}

// ----------------------------------------------------------------------
// Pure-function tests — match decode (decode_match_fields)
// ----------------------------------------------------------------------

#[test]
fn decode_match_passes_id_and_score_through() {
    let m = PineconeVectorDriver::decode_match_fields("my-id".to_string(), 0.93, None);
    assert_eq!(m.id, "my-id");
    assert!((m.score - 0.93).abs() < 1e-6);
    assert_eq!(m.metadata, serde_json::Value::Null);
}

#[test]
fn decode_match_with_metadata_yields_object() {
    let mut fields = std::collections::BTreeMap::new();
    fields.insert(
        "key".to_string(),
        PbValue {
            kind: Some(Kind::StringValue("value".into())),
        },
    );
    let m =
        PineconeVectorDriver::decode_match_fields("id".to_string(), 0.5, Some(PbStruct { fields }));
    assert_eq!(m.metadata["key"], "value");
}

// ----------------------------------------------------------------------
// Pure-function tests — short-circuits (no real network)
// ----------------------------------------------------------------------

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
    assert!(format!("{err}").contains("zero-length"));
}

// ----------------------------------------------------------------------
// Pure-function tests — builder
// ----------------------------------------------------------------------

#[test]
fn with_namespace_sets_the_namespace() {
    let driver = unreachable_driver().with_namespace("custom-ns");
    assert_eq!(driver.namespace().name, "custom-ns");
}

#[test]
fn default_namespace_is_empty() {
    let driver = unreachable_driver();
    assert_eq!(driver.namespace().name, "");
}

// ----------------------------------------------------------------------
// Integration tests — env-gated, require a real Pinecone account
// ----------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PINECONE_API_KEY and PINECONE_TEST_INDEX"]
async fn integration_upsert_and_count_roundtrip() {
    let Some((key, index_name)) = pinecone_env_or_skip("upsert_and_count_roundtrip") else {
        return;
    };
    let ns = unique_namespace("count");
    let driver = PineconeVectorDriver::from_api_key(&key)
        .unwrap()
        .with_namespace(&ns);

    // Use 4-dim vectors — common for small test indexes. If the
    // user's index has a different dim, this test will error from
    // Pinecone and surface a clear server error in the assertion.
    driver
        .upsert(
            &index_name,
            vec![
                VectorItem::new("a", vec![1.0, 0.0, 0.0, 0.0], serde_json::json!({"tag": 1})),
                VectorItem::new("b", vec![0.0, 1.0, 0.0, 0.0], serde_json::json!({"tag": 2})),
            ],
        )
        .await
        .expect("upsert succeeds; if your index dim != 4 this is the failure surface");

    // Pinecone's stats endpoint is eventually consistent; poll
    // briefly so flakes don't fail the test.
    let mut count = 0;
    for _ in 0..10 {
        count = driver.count(&index_name).await.unwrap();
        if count >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert_eq!(count, 2, "two vectors should be present in the namespace");

    delete_namespace(&driver, &index_name, &ns).await;
}

#[tokio::test]
#[ignore = "requires PINECONE_API_KEY and PINECONE_TEST_INDEX"]
async fn integration_similar_returns_top_k_descending() {
    let Some((key, index_name)) = pinecone_env_or_skip("similar_top_k") else {
        return;
    };
    let ns = unique_namespace("topk");
    let driver = PineconeVectorDriver::from_api_key(&key)
        .unwrap()
        .with_namespace(&ns);

    driver
        .upsert(
            &index_name,
            vec![
                VectorItem::new("perfect", vec![1.0, 0.0, 0.0, 0.0], serde_json::json!({})),
                VectorItem::new(
                    "orthogonal",
                    vec![0.0, 1.0, 0.0, 0.0],
                    serde_json::json!({}),
                ),
                VectorItem::new("close", vec![0.9, 0.1, 0.0, 0.0], serde_json::json!({})),
            ],
        )
        .await
        .unwrap();

    // Poll for index propagation
    let mut hits = vec![];
    for _ in 0..10 {
        hits = driver
            .similar(&index_name, vec![1.0, 0.0, 0.0, 0.0], 3)
            .await
            .unwrap();
        if hits.len() == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].id, "perfect");
    assert!(hits[0].score >= hits[1].score);
    assert!(hits[1].score >= hits[2].score);

    delete_namespace(&driver, &index_name, &ns).await;
}

#[tokio::test]
#[ignore = "requires PINECONE_API_KEY and PINECONE_TEST_INDEX"]
async fn integration_delete_removes_points_by_id() {
    let Some((key, index_name)) = pinecone_env_or_skip("delete_by_id") else {
        return;
    };
    let ns = unique_namespace("del");
    let driver = PineconeVectorDriver::from_api_key(&key)
        .unwrap()
        .with_namespace(&ns);

    driver
        .upsert(
            &index_name,
            vec![
                VectorItem::new("keep", vec![1.0, 0.0, 0.0, 0.0], serde_json::json!({})),
                VectorItem::new("toss", vec![0.0, 1.0, 0.0, 0.0], serde_json::json!({})),
            ],
        )
        .await
        .unwrap();
    driver
        .delete(&index_name, vec!["toss".to_string()])
        .await
        .unwrap();

    let mut count = 99;
    for _ in 0..10 {
        count = driver.count(&index_name).await.unwrap();
        if count <= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert_eq!(count, 1);

    delete_namespace(&driver, &index_name, &ns).await;
}

#[tokio::test]
#[ignore = "requires PINECONE_API_KEY and PINECONE_TEST_INDEX"]
async fn integration_metadata_roundtrips_through_pinecone() {
    let Some((key, index_name)) = pinecone_env_or_skip("metadata_roundtrip") else {
        return;
    };
    let ns = unique_namespace("meta");
    let driver = PineconeVectorDriver::from_api_key(&key)
        .unwrap()
        .with_namespace(&ns);

    driver
        .upsert(
            &index_name,
            vec![VectorItem::new(
                "doc-1",
                vec![1.0, 0.0, 0.0, 0.0],
                serde_json::json!({
                    "title": "Hello",
                    "score_field": 4.5,
                    "active": true
                }),
            )],
        )
        .await
        .unwrap();

    let mut hits = vec![];
    for _ in 0..10 {
        hits = driver
            .similar(&index_name, vec![1.0, 0.0, 0.0, 0.0], 1)
            .await
            .unwrap();
        if !hits.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "doc-1");
    assert_eq!(hits[0].metadata["title"], "Hello");
    assert_eq!(hits[0].metadata["score_field"], 4.5);
    assert_eq!(hits[0].metadata["active"], true);

    delete_namespace(&driver, &index_name, &ns).await;
}
