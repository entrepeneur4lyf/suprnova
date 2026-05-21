//! Phase 9A — Memory vector driver smoke tests.
//!
//! Pins the trait contract end-to-end via the in-process driver:
//! - upsert round-trips
//! - upsert by same id replaces
//! - similar returns top-k ordered best-first
//! - similar on empty store returns Vec::new()
//! - delete removes by id
//! - count tracks the live point set
//! - similar with dimension mismatch silently skips (no crash)
//! - similar with zero-vector query errors clearly
//! - Vector::store on unregistered name returns 404-mapped error

use std::sync::Arc;
use suprnova::{MemoryVectorDriver, Vector, VectorItem};

fn unique_store_name(tag: &str) -> String {
    // Each test uses a distinct store name so parallel runs don't
    // collide on the process-global VectorRegistry.
    format!("p9a-{tag}-{}", std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos())
}

#[tokio::test]
async fn upsert_and_count_roundtrip() {
    let name = unique_store_name("upsert");
    Vector::register(name.clone(), Arc::new(MemoryVectorDriver::new()));
    let store = Vector::store(&name).unwrap();
    store
        .upsert(vec![
            VectorItem::new("a", vec![1.0, 0.0, 0.0], serde_json::json!({"tag": 1})),
            VectorItem::new("b", vec![0.0, 1.0, 0.0], serde_json::json!({"tag": 2})),
        ])
        .await
        .unwrap();
    assert_eq!(store.count().await.unwrap(), 2);
}

#[tokio::test]
async fn upsert_same_id_replaces_existing_point() {
    let name = unique_store_name("replace");
    Vector::register(name.clone(), Arc::new(MemoryVectorDriver::new()));
    let store = Vector::store(&name).unwrap();
    store
        .upsert(vec![VectorItem::new("x", vec![1.0, 0.0], serde_json::json!({"v": 1}))])
        .await
        .unwrap();
    store
        .upsert(vec![VectorItem::new("x", vec![0.0, 1.0], serde_json::json!({"v": 2}))])
        .await
        .unwrap();
    assert_eq!(store.count().await.unwrap(), 1, "same id must merge");
    let hits = store.similar(vec![0.0, 1.0], 1).await.unwrap();
    assert_eq!(hits[0].id, "x");
    assert_eq!(hits[0].metadata["v"], 2, "metadata follows the new embedding");
}

#[tokio::test]
async fn similar_returns_top_k_in_descending_score_order() {
    let name = unique_store_name("topk");
    Vector::register(name.clone(), Arc::new(MemoryVectorDriver::new()));
    let store = Vector::store(&name).unwrap();
    store
        .upsert(vec![
            VectorItem::new("perfect", vec![1.0, 0.0, 0.0], serde_json::json!({})),
            VectorItem::new("orthogonal", vec![0.0, 1.0, 0.0], serde_json::json!({})),
            VectorItem::new("close", vec![0.9, 0.1, 0.0], serde_json::json!({})),
        ])
        .await
        .unwrap();
    let hits = store.similar(vec![1.0, 0.0, 0.0], 3).await.unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].id, "perfect");
    assert_eq!(hits[1].id, "close");
    assert_eq!(hits[2].id, "orthogonal");
    assert!(hits[0].score > hits[1].score);
    assert!(hits[1].score > hits[2].score);
}

#[tokio::test]
async fn similar_on_empty_store_returns_empty_vec() {
    let name = unique_store_name("empty");
    Vector::register(name.clone(), Arc::new(MemoryVectorDriver::new()));
    let store = Vector::store(&name).unwrap();
    let hits = store.similar(vec![1.0, 2.0, 3.0], 5).await.unwrap();
    assert!(hits.is_empty());
}

#[tokio::test]
async fn delete_removes_points_by_id() {
    let name = unique_store_name("del");
    Vector::register(name.clone(), Arc::new(MemoryVectorDriver::new()));
    let store = Vector::store(&name).unwrap();
    store
        .upsert(vec![
            VectorItem::new("keep", vec![1.0, 0.0], serde_json::json!({})),
            VectorItem::new("toss", vec![0.0, 1.0], serde_json::json!({})),
        ])
        .await
        .unwrap();
    store.delete(["toss"]).await.unwrap();
    assert_eq!(store.count().await.unwrap(), 1);
    let hits = store.similar(vec![0.0, 1.0], 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "keep");
}

#[tokio::test]
async fn similar_skips_mismatched_dimension_silently() {
    let name = unique_store_name("dimskip");
    Vector::register(name.clone(), Arc::new(MemoryVectorDriver::new()));
    let store = Vector::store(&name).unwrap();
    store
        .upsert(vec![
            VectorItem::new("d2", vec![1.0, 0.0], serde_json::json!({})),
            VectorItem::new("d3", vec![1.0, 0.0, 0.0], serde_json::json!({})),
        ])
        .await
        .unwrap();
    // Query is 2-dim — only the d2 point matches the dimension.
    let hits = store.similar(vec![1.0, 0.0], 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "d2");
}

#[tokio::test]
async fn similar_with_zero_vector_query_errors() {
    let name = unique_store_name("zero");
    Vector::register(name.clone(), Arc::new(MemoryVectorDriver::new()));
    let store = Vector::store(&name).unwrap();
    store
        .upsert(vec![VectorItem::new("a", vec![1.0, 0.0], serde_json::json!({}))])
        .await
        .unwrap();
    let err = store.similar(vec![0.0, 0.0], 1).await.unwrap_err();
    assert!(
        format!("{err}").contains("zero-vector"),
        "error names the cause: {err}"
    );
}

#[tokio::test]
async fn store_lookup_for_unregistered_name_returns_not_found() {
    match Vector::store("never-registered-xyz-9A") {
        Ok(_) => panic!("expected an error for an unregistered store"),
        Err(err) => {
            let msg = format!("{err}");
            assert!(msg.contains("never-registered-xyz-9A"));
            assert!(msg.contains("not registered"));
        }
    }
}
