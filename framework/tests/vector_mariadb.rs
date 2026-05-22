//! Phase 9B — MariaDB vector driver tests.
//!
//! Two layers:
//!
//! 1. **Pure-function tests** (no `#[ignore]`) — always run; cover
//!    embedding formatting, store-name validation, score normalization,
//!    `ensure_table_sql` output, short-circuit paths, and metadata
//!    rejection. Construction uses
//!    [`MariaDbVectorDriver::from_url`] against a never-reachable
//!    address — the lazy pool never connects, so these tests don't need
//!    a live MariaDB.
//!
//! 2. **Integration tests** (`#[ignore]`) — exercise the driver against
//!    a real MariaDB 11.7+. Skipped by default; run via:
//!
//!    ```bash
//!    docker run -p 3306:3306 \
//!        -e MARIADB_ROOT_PASSWORD=secret \
//!        -e MARIADB_DATABASE=vectors \
//!        mariadb:11.7
//!    MARIADB_URL='mysql://root:secret@localhost:3306/vectors' \
//!        cargo test -p suprnova --test vector_mariadb -- --ignored
//!    ```
//!
//!    Each test uses a unique table name (`p9b_<tag>_<nanos>`) and
//!    `DROP TABLE`s on the way out so parallel runs don't clash.

use std::time::UNIX_EPOCH;
use suprnova::vector::driver::VectorDriver;
use suprnova::{MariaDbDistance, MariaDbVectorDriver, VectorItem};

fn unreachable_driver() -> MariaDbVectorDriver {
    // 0.0.0.0:1 is guaranteed-unreachable in practice; `connect_lazy`
    // doesn't connect anyway, so this just builds the wrapper struct.
    MariaDbVectorDriver::from_url("mysql://x:y@0.0.0.0:1/db")
        .expect("connect_lazy validates URL syntax, not reachability")
}

fn unique_table(tag: &str) -> String {
    format!(
        "p9b_{tag}_{}",
        UNIX_EPOCH.elapsed().unwrap().as_nanos()
    )
}

fn mariadb_url_or_skip(test_name: &str) -> Option<String> {
    match std::env::var("MARIADB_URL") {
        Ok(url) => Some(url),
        Err(_) => {
            eprintln!("[{test_name}] skipping: MARIADB_URL not set");
            None
        }
    }
}

async fn drop_table(driver: &MariaDbVectorDriver, table: &str) {
    let sql = format!("DROP TABLE IF EXISTS `{table}`");
    let _ = sqlx::query(&sql).execute(driver.pool()).await;
}

// ---------------------------------------------------------------------
// Pure-function tests — distance enum surface
// ---------------------------------------------------------------------

#[test]
fn distance_default_is_cosine() {
    assert_eq!(MariaDbDistance::default(), MariaDbDistance::Cosine);
}

#[test]
fn distance_index_clause_is_lowercase_keyword() {
    assert_eq!(MariaDbDistance::Cosine.index_clause(), "cosine");
    assert_eq!(MariaDbDistance::Euclidean.index_clause(), "euclidean");
}

#[test]
fn distance_fn_name_matches_mariadb_builtins() {
    assert_eq!(
        MariaDbDistance::Cosine.fn_name(),
        "VEC_DISTANCE_COSINE"
    );
    assert_eq!(
        MariaDbDistance::Euclidean.fn_name(),
        "VEC_DISTANCE_EUCLIDEAN"
    );
}

#[tokio::test]
async fn with_distance_sets_metric() {
    // `connect_lazy` registers itself with the Tokio runtime even
    // though it doesn't connect; this test needs an async context.
    let driver = unreachable_driver().with_distance(MariaDbDistance::Euclidean);
    assert_eq!(driver.distance(), MariaDbDistance::Euclidean);
}

#[tokio::test]
async fn default_driver_uses_cosine() {
    let driver = unreachable_driver();
    assert_eq!(driver.distance(), MariaDbDistance::Cosine);
}

// ---------------------------------------------------------------------
// Pure-function tests — store name validation
// ---------------------------------------------------------------------

#[test]
fn validate_store_name_accepts_simple_ident() {
    assert!(MariaDbVectorDriver::validate_store_name("documents").is_ok());
}

#[test]
fn validate_store_name_accepts_underscore_first() {
    assert!(MariaDbVectorDriver::validate_store_name("_internal").is_ok());
}

#[test]
fn validate_store_name_accepts_digits_after_first() {
    assert!(MariaDbVectorDriver::validate_store_name("docs_v2").is_ok());
}

#[test]
fn validate_store_name_rejects_empty() {
    let err = MariaDbVectorDriver::validate_store_name("").unwrap_err();
    assert!(err.to_string().to_lowercase().contains("empty"));
}

#[test]
fn validate_store_name_rejects_leading_digit() {
    assert!(MariaDbVectorDriver::validate_store_name("9docs").is_err());
}

#[test]
fn validate_store_name_rejects_backtick() {
    let err = MariaDbVectorDriver::validate_store_name("docs`; DROP TABLE x; --").unwrap_err();
    assert!(err.to_string().contains("invalid character"));
}

#[test]
fn validate_store_name_rejects_semicolon() {
    assert!(MariaDbVectorDriver::validate_store_name("docs;").is_err());
}

#[test]
fn validate_store_name_rejects_space() {
    assert!(MariaDbVectorDriver::validate_store_name("doc store").is_err());
}

#[test]
fn validate_store_name_rejects_dash() {
    assert!(MariaDbVectorDriver::validate_store_name("doc-store").is_err());
}

#[test]
fn validate_store_name_rejects_dot() {
    // Avoids "db.table" two-component identifiers — keep it single-name.
    assert!(MariaDbVectorDriver::validate_store_name("db.docs").is_err());
}

#[test]
fn validate_store_name_rejects_overlong() {
    let long = "x".repeat(65);
    let err = MariaDbVectorDriver::validate_store_name(&long).unwrap_err();
    assert!(err.to_string().contains("64"));
}

#[test]
fn validate_store_name_accepts_64_chars() {
    let exact = "x".repeat(64);
    assert!(MariaDbVectorDriver::validate_store_name(&exact).is_ok());
}

// ---------------------------------------------------------------------
// Pure-function tests — embedding to VEC_FROMTEXT format
// ---------------------------------------------------------------------

#[test]
fn embedding_to_vec_text_basic() {
    let out = MariaDbVectorDriver::embedding_to_vec_text(&[1.0, 2.0, 3.0]).unwrap();
    assert_eq!(out, "[1,2,3]");
}

#[test]
fn embedding_to_vec_text_preserves_decimals() {
    let out = MariaDbVectorDriver::embedding_to_vec_text(&[1.5, -0.25, 0.125]).unwrap();
    assert_eq!(out, "[1.5,-0.25,0.125]");
}

#[test]
fn embedding_to_vec_text_handles_zero() {
    let out = MariaDbVectorDriver::embedding_to_vec_text(&[0.0, 0.0]).unwrap();
    assert_eq!(out, "[0,0]");
}

#[test]
fn embedding_to_vec_text_handles_negatives() {
    let out = MariaDbVectorDriver::embedding_to_vec_text(&[-1.0, -2.5, -0.0001]).unwrap();
    assert_eq!(out, "[-1,-2.5,-0.0001]");
}

#[test]
fn embedding_to_vec_text_handles_scientific_notation() {
    // Small values trigger Rust's exponential Display path. JSON
    // permits this form; MariaDB's VEC_FROMTEXT parses JSON numbers
    // so it must accept this too.
    let out = MariaDbVectorDriver::embedding_to_vec_text(&[1e-30, 1e30]).unwrap();
    // The exact string is Rust-version-dependent for tiny floats but
    // must be JSON-number-shaped: digits-then-optional-exponent.
    // We assert the pieces by parsing back through serde_json.
    let parsed: serde_json::Value = serde_json::from_str(&out)
        .expect("output must be valid JSON");
    let arr = parsed.as_array().expect("must be a JSON array");
    assert_eq!(arr.len(), 2);
    assert!(arr[0].is_number());
    assert!(arr[1].is_number());
}

#[test]
fn embedding_to_vec_text_rejects_empty() {
    let err = MariaDbVectorDriver::embedding_to_vec_text(&[]).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("empty"));
}

#[test]
fn embedding_to_vec_text_rejects_nan() {
    let err = MariaDbVectorDriver::embedding_to_vec_text(&[1.0, f32::NAN]).unwrap_err();
    assert!(err.to_string().contains("non-finite"));
    assert!(err.to_string().contains("index 1"));
}

#[test]
fn embedding_to_vec_text_rejects_infinity() {
    assert!(MariaDbVectorDriver::embedding_to_vec_text(&[f32::INFINITY]).is_err());
    assert!(MariaDbVectorDriver::embedding_to_vec_text(&[f32::NEG_INFINITY]).is_err());
}

#[test]
fn embedding_to_vec_text_output_is_valid_json_array() {
    // Property check: any finite f32 slice must produce valid JSON.
    let cases: &[&[f32]] = &[
        &[1.0],
        &[0.0, 0.0, 0.0],
        &[1.0, 2.0, 3.0, 4.0, 5.0],
        &[-1.0, -2.0, -3.0],
        &[1e-20, 1.5, 1e10],
        &[f32::MIN_POSITIVE, f32::MAX, -f32::MAX],
    ];
    for v in cases {
        let out = MariaDbVectorDriver::embedding_to_vec_text(v).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("invalid JSON for {v:?}: {out:?} ({e})"));
        let arr = parsed.as_array().expect("must be array");
        assert_eq!(arr.len(), v.len(), "length mismatch for {v:?}");
    }
}

// ---------------------------------------------------------------------
// Pure-function tests — score normalization
// ---------------------------------------------------------------------

#[test]
fn cosine_score_d0_is_one() {
    let s = MariaDbVectorDriver::score_from_distance(MariaDbDistance::Cosine, 0.0);
    assert!((s - 1.0).abs() < 1e-6, "cosine d=0 should map to 1.0, got {s}");
}

#[test]
fn cosine_score_d1_is_half() {
    let s = MariaDbVectorDriver::score_from_distance(MariaDbDistance::Cosine, 1.0);
    assert!((s - 0.5).abs() < 1e-6, "cosine d=1 should map to 0.5, got {s}");
}

#[test]
fn cosine_score_d2_is_zero() {
    let s = MariaDbVectorDriver::score_from_distance(MariaDbDistance::Cosine, 2.0);
    assert!(s.abs() < 1e-6, "cosine d=2 should map to 0.0, got {s}");
}

#[test]
fn cosine_score_clamps_above_two() {
    // Float drift can put d a hair past the theoretical max; we clamp.
    let s = MariaDbVectorDriver::score_from_distance(MariaDbDistance::Cosine, 2.001);
    assert_eq!(s, 0.0);
}

#[test]
fn cosine_score_clamps_negative() {
    let s = MariaDbVectorDriver::score_from_distance(MariaDbDistance::Cosine, -0.001);
    assert!(
        (s - 1.0).abs() < 1e-6,
        "negative cosine distance should clamp to score 1.0, got {s}"
    );
}

#[test]
fn euclidean_score_d0_is_one() {
    let s = MariaDbVectorDriver::score_from_distance(MariaDbDistance::Euclidean, 0.0);
    assert!((s - 1.0).abs() < 1e-6);
}

#[test]
fn euclidean_score_d1_is_half() {
    let s = MariaDbVectorDriver::score_from_distance(MariaDbDistance::Euclidean, 1.0);
    assert!((s - 0.5).abs() < 1e-6);
}

#[test]
fn euclidean_score_d9_is_one_tenth() {
    let s = MariaDbVectorDriver::score_from_distance(MariaDbDistance::Euclidean, 9.0);
    assert!((s - 0.1).abs() < 1e-6, "euclidean d=9 should map to 0.1, got {s}");
}

#[test]
fn euclidean_score_large_d_approaches_zero() {
    let s = MariaDbVectorDriver::score_from_distance(MariaDbDistance::Euclidean, 1e9);
    assert!(s < 1e-6, "large euclidean distance should approach 0, got {s}");
    assert!(s >= 0.0, "score should never go negative");
}

#[test]
fn euclidean_score_clamps_negative() {
    let s = MariaDbVectorDriver::score_from_distance(MariaDbDistance::Euclidean, -1.0);
    assert!(
        (s - 1.0).abs() < 1e-6,
        "negative euclidean distance should clamp to score 1.0, got {s}"
    );
}

#[test]
fn score_is_monotone_decreasing_in_distance() {
    // Property: bigger distance → smaller score, both metrics.
    for metric in [MariaDbDistance::Cosine, MariaDbDistance::Euclidean] {
        let distances = [0.0, 0.5, 1.0, 1.5, 2.0, 5.0];
        let mut prev = f32::INFINITY;
        for &d in &distances {
            let s = MariaDbVectorDriver::score_from_distance(metric, d);
            assert!(
                s <= prev,
                "{metric:?}: score not monotone at d={d}: prev={prev}, current={s}"
            );
            prev = s;
        }
    }
}

// ---------------------------------------------------------------------
// Pure-function tests — ensure_table_sql shape
// ---------------------------------------------------------------------

#[test]
fn ensure_table_sql_cosine_shape() {
    let sql = MariaDbVectorDriver::ensure_table_sql("documents", 1536, MariaDbDistance::Cosine)
        .unwrap();
    assert!(sql.contains("CREATE TABLE IF NOT EXISTS `documents`"));
    assert!(sql.contains("id VARCHAR(255) NOT NULL PRIMARY KEY"));
    assert!(sql.contains("embedding VECTOR(1536) NOT NULL"));
    assert!(sql.contains("metadata JSON NULL"));
    assert!(sql.contains("VECTOR INDEX (embedding) DISTANCE=cosine"));
    assert!(sql.contains("ENGINE=InnoDB"));
    assert!(sql.contains("utf8mb4"));
}

#[test]
fn ensure_table_sql_euclidean_clause() {
    let sql =
        MariaDbVectorDriver::ensure_table_sql("vecs", 128, MariaDbDistance::Euclidean).unwrap();
    assert!(sql.contains("DISTANCE=euclidean"));
    assert!(!sql.contains("cosine"));
}

#[test]
fn ensure_table_sql_quotes_table_name_in_backticks() {
    let sql = MariaDbVectorDriver::ensure_table_sql("foo", 4, MariaDbDistance::Cosine).unwrap();
    // Backticks defend against future name changes that bypass our validation.
    assert!(sql.contains("`foo`"));
}

#[test]
fn ensure_table_sql_rejects_invalid_name() {
    let err = MariaDbVectorDriver::ensure_table_sql(
        "docs; DROP TABLE x; --",
        128,
        MariaDbDistance::Cosine,
    )
    .unwrap_err();
    assert!(err.to_string().contains("invalid character"));
}

#[test]
fn ensure_table_sql_rejects_zero_dim() {
    let err =
        MariaDbVectorDriver::ensure_table_sql("docs", 0, MariaDbDistance::Cosine).unwrap_err();
    assert!(err.to_string().contains("dim"));
}

#[tokio::test]
async fn ensure_table_sql_for_inherits_driver_distance_cosine() {
    let driver = unreachable_driver().with_distance(MariaDbDistance::Cosine);
    let sql = driver.ensure_table_sql_for("docs", 64).unwrap();
    assert!(sql.contains("DISTANCE=cosine"));
    assert!(!sql.contains("DISTANCE=euclidean"));
}

#[tokio::test]
async fn ensure_table_sql_for_inherits_driver_distance_euclidean() {
    let driver = unreachable_driver().with_distance(MariaDbDistance::Euclidean);
    let sql = driver.ensure_table_sql_for("docs", 64).unwrap();
    assert!(sql.contains("DISTANCE=euclidean"));
    assert!(!sql.contains("DISTANCE=cosine"));
}

#[tokio::test]
async fn ensure_table_sql_for_validates_name_and_dim() {
    let driver = unreachable_driver();
    assert!(driver.ensure_table_sql_for("docs;", 64).is_err());
    assert!(driver.ensure_table_sql_for("docs", 0).is_err());
}

// ---------------------------------------------------------------------
// Trait short-circuits (no DB connection)
// ---------------------------------------------------------------------

#[tokio::test]
async fn upsert_empty_items_is_noop() {
    let driver = unreachable_driver();
    // No version check, no SQL — short-circuits before either.
    driver
        .upsert("documents", vec![])
        .await
        .expect("empty upsert must short-circuit before touching the network");
}

#[tokio::test]
async fn delete_empty_ids_is_noop() {
    let driver = unreachable_driver();
    driver
        .delete("documents", vec![])
        .await
        .expect("empty delete must short-circuit");
}

#[tokio::test]
async fn similar_with_k_zero_returns_empty() {
    let driver = unreachable_driver();
    let out = driver
        .similar("documents", vec![1.0, 2.0, 3.0], 0)
        .await
        .expect("k=0 must short-circuit");
    assert!(out.is_empty());
}

#[tokio::test]
async fn similar_with_empty_query_returns_empty() {
    let driver = unreachable_driver();
    let out = driver
        .similar("documents", vec![], 5)
        .await
        .expect("empty query must short-circuit");
    assert!(out.is_empty());
}

#[tokio::test]
async fn similar_with_zero_vector_query_errors() {
    let driver = unreachable_driver();
    let err = driver
        .similar("documents", vec![0.0, 0.0, 0.0], 5)
        .await
        .expect_err("zero-vector query must error before SQL");
    assert!(err.to_string().to_lowercase().contains("zero-vector"));
}

// ---------------------------------------------------------------------
// Metadata validation (no DB connection)
// ---------------------------------------------------------------------

#[tokio::test]
async fn upsert_rejects_array_metadata() {
    let driver = unreachable_driver();
    let item = VectorItem::new(
        "doc-1",
        vec![1.0, 2.0],
        serde_json::json!(["not", "an", "object"]),
    );
    let err = driver
        .upsert("documents", vec![item])
        .await
        .expect_err("array metadata must be rejected");
    assert!(err.to_string().contains("metadata must be a JSON object or null"));
}

#[tokio::test]
async fn upsert_rejects_primitive_metadata() {
    let driver = unreachable_driver();
    let item = VectorItem::new("doc-1", vec![1.0, 2.0], serde_json::json!(42));
    assert!(driver.upsert("documents", vec![item]).await.is_err());
}

#[tokio::test]
async fn upsert_rejects_invalid_store_name() {
    let driver = unreachable_driver();
    let item = VectorItem::new("doc-1", vec![1.0, 2.0], serde_json::Value::Null);
    let err = driver
        .upsert("docs; DROP TABLE x; --", vec![item])
        .await
        .expect_err("must reject SQL-shaped store name");
    assert!(err.to_string().contains("invalid character"));
}

#[tokio::test]
async fn delete_rejects_invalid_store_name() {
    let driver = unreachable_driver();
    let err = driver
        .delete("docs;", vec!["1".to_string()])
        .await
        .expect_err("must reject invalid store name");
    assert!(err.to_string().contains("invalid character"));
}

// =====================================================================
// Integration tests (MARIADB_URL, MariaDB 11.7+)
// =====================================================================

#[tokio::test]
#[ignore]
async fn integration_version_check_passes_on_117() {
    let url = match mariadb_url_or_skip("integration_version_check_passes_on_117") {
        Some(u) => u,
        None => return,
    };
    let driver = MariaDbVectorDriver::from_url(&url).unwrap();
    // First call runs SELECT VERSION() and caches.
    let out = driver.count("does_not_exist_table_used_to_trigger_version_check").await;
    // We expect failure because the table doesn't exist — but the
    // error must be a SQL error, NOT a version-rejection error. If
    // the server is < 11.7 the test would fail here with the
    // version-rejection message instead.
    let err = out.expect_err("count against nonexistent table must error");
    let msg = err.to_string();
    assert!(
        !msg.contains("requires MariaDB 11.7+"),
        "MariaDB version is < 11.7 — bump the test fixture: {msg}"
    );
}

#[tokio::test]
#[ignore]
async fn integration_upsert_count_roundtrip_cosine() {
    let url = match mariadb_url_or_skip("integration_upsert_count_roundtrip_cosine") {
        Some(u) => u,
        None => return,
    };
    let driver = MariaDbVectorDriver::from_url(&url).unwrap();
    let table = unique_table("rt_cos");
    drop_table(&driver, &table).await;

    let create_sql =
        MariaDbVectorDriver::ensure_table_sql(&table, 3, MariaDbDistance::Cosine).unwrap();
    sqlx::query(&create_sql)
        .execute(driver.pool())
        .await
        .expect("CREATE TABLE");

    let items = vec![
        VectorItem::new("a", vec![1.0, 0.0, 0.0], serde_json::json!({"label": "x"})),
        VectorItem::new("b", vec![0.0, 1.0, 0.0], serde_json::json!({"label": "y"})),
        VectorItem::new("c", vec![0.0, 0.0, 1.0], serde_json::Value::Null),
    ];
    driver.upsert(&table, items).await.expect("upsert ok");
    assert_eq!(driver.count(&table).await.unwrap(), 3);
    drop_table(&driver, &table).await;
}

#[tokio::test]
#[ignore]
async fn integration_upsert_replaces_on_duplicate_key() {
    let url = match mariadb_url_or_skip("integration_upsert_replaces_on_duplicate_key") {
        Some(u) => u,
        None => return,
    };
    let driver = MariaDbVectorDriver::from_url(&url).unwrap();
    let table = unique_table("dup");
    drop_table(&driver, &table).await;

    let create_sql =
        MariaDbVectorDriver::ensure_table_sql(&table, 3, MariaDbDistance::Cosine).unwrap();
    sqlx::query(&create_sql)
        .execute(driver.pool())
        .await
        .expect("CREATE TABLE");

    driver
        .upsert(
            &table,
            vec![VectorItem::new(
                "a",
                vec![1.0, 0.0, 0.0],
                serde_json::json!({"v": 1}),
            )],
        )
        .await
        .unwrap();
    driver
        .upsert(
            &table,
            vec![VectorItem::new(
                "a",
                vec![0.0, 1.0, 0.0],
                serde_json::json!({"v": 2}),
            )],
        )
        .await
        .unwrap();
    assert_eq!(driver.count(&table).await.unwrap(), 1);

    let hits = driver
        .similar(&table, vec![0.0, 1.0, 0.0], 1)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "a");
    assert_eq!(
        hits[0].metadata,
        serde_json::json!({"v": 2}),
        "replacement must have new metadata"
    );

    drop_table(&driver, &table).await;
}

#[tokio::test]
#[ignore]
async fn integration_similar_cosine_returns_best_match_first() {
    let url = match mariadb_url_or_skip("integration_similar_cosine_returns_best_match_first") {
        Some(u) => u,
        None => return,
    };
    let driver = MariaDbVectorDriver::from_url(&url).unwrap();
    let table = unique_table("simcos");
    drop_table(&driver, &table).await;

    let create_sql =
        MariaDbVectorDriver::ensure_table_sql(&table, 3, MariaDbDistance::Cosine).unwrap();
    sqlx::query(&create_sql)
        .execute(driver.pool())
        .await
        .expect("CREATE TABLE");

    driver
        .upsert(
            &table,
            vec![
                VectorItem::new("near", vec![1.0, 0.0, 0.0], serde_json::Value::Null),
                VectorItem::new("middle", vec![1.0, 1.0, 0.0], serde_json::Value::Null),
                VectorItem::new("far", vec![0.0, 0.0, 1.0], serde_json::Value::Null),
            ],
        )
        .await
        .unwrap();

    let hits = driver
        .similar(&table, vec![1.0, 0.0, 0.0], 3)
        .await
        .unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].id, "near");
    assert_eq!(hits[2].id, "far");
    // Scores should be monotone-decreasing in best-first order.
    assert!(hits[0].score >= hits[1].score);
    assert!(hits[1].score >= hits[2].score);
    // Cosine score for the exact-match query against itself is 1.0.
    assert!(
        hits[0].score > 0.99,
        "exact-match cosine score should be ~1.0, got {}",
        hits[0].score
    );

    drop_table(&driver, &table).await;
}

#[tokio::test]
#[ignore]
async fn integration_similar_euclidean_returns_best_match_first() {
    let url = match mariadb_url_or_skip("integration_similar_euclidean_returns_best_match_first") {
        Some(u) => u,
        None => return,
    };
    let driver = MariaDbVectorDriver::from_url(&url)
        .unwrap()
        .with_distance(MariaDbDistance::Euclidean);
    let table = unique_table("simeuc");
    drop_table(&driver, &table).await;

    let create_sql =
        MariaDbVectorDriver::ensure_table_sql(&table, 3, MariaDbDistance::Euclidean).unwrap();
    sqlx::query(&create_sql)
        .execute(driver.pool())
        .await
        .expect("CREATE TABLE");

    driver
        .upsert(
            &table,
            vec![
                VectorItem::new("origin", vec![0.0, 0.0, 0.0], serde_json::Value::Null),
                VectorItem::new("close", vec![0.1, 0.0, 0.0], serde_json::Value::Null),
                VectorItem::new("far", vec![10.0, 10.0, 10.0], serde_json::Value::Null),
            ],
        )
        .await
        .unwrap();

    let hits = driver
        .similar(&table, vec![0.0, 0.0, 0.0], 3)
        .await
        .unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].id, "origin");
    assert_eq!(hits[1].id, "close");
    assert_eq!(hits[2].id, "far");
    assert!(hits[0].score >= hits[1].score);
    assert!(hits[1].score >= hits[2].score);

    drop_table(&driver, &table).await;
}

#[tokio::test]
#[ignore]
async fn integration_similar_respects_k_limit() {
    let url = match mariadb_url_or_skip("integration_similar_respects_k_limit") {
        Some(u) => u,
        None => return,
    };
    let driver = MariaDbVectorDriver::from_url(&url).unwrap();
    let table = unique_table("klim");
    drop_table(&driver, &table).await;

    let create_sql =
        MariaDbVectorDriver::ensure_table_sql(&table, 3, MariaDbDistance::Cosine).unwrap();
    sqlx::query(&create_sql)
        .execute(driver.pool())
        .await
        .expect("CREATE TABLE");

    let items: Vec<VectorItem> = (0..5)
        .map(|i| {
            VectorItem::new(
                format!("v{i}"),
                vec![i as f32, (i + 1) as f32, (i + 2) as f32],
                serde_json::Value::Null,
            )
        })
        .collect();
    driver.upsert(&table, items).await.unwrap();

    let hits = driver
        .similar(&table, vec![1.0, 2.0, 3.0], 2)
        .await
        .unwrap();
    assert_eq!(hits.len(), 2, "k=2 must limit to 2 hits");

    drop_table(&driver, &table).await;
}

#[tokio::test]
#[ignore]
async fn integration_delete_removes_by_id() {
    let url = match mariadb_url_or_skip("integration_delete_removes_by_id") {
        Some(u) => u,
        None => return,
    };
    let driver = MariaDbVectorDriver::from_url(&url).unwrap();
    let table = unique_table("del");
    drop_table(&driver, &table).await;

    let create_sql =
        MariaDbVectorDriver::ensure_table_sql(&table, 3, MariaDbDistance::Cosine).unwrap();
    sqlx::query(&create_sql)
        .execute(driver.pool())
        .await
        .expect("CREATE TABLE");

    driver
        .upsert(
            &table,
            vec![
                VectorItem::new("keep", vec![1.0, 0.0, 0.0], serde_json::Value::Null),
                VectorItem::new("drop", vec![0.0, 1.0, 0.0], serde_json::Value::Null),
            ],
        )
        .await
        .unwrap();
    assert_eq!(driver.count(&table).await.unwrap(), 2);

    driver
        .delete(&table, vec!["drop".to_string()])
        .await
        .unwrap();
    assert_eq!(driver.count(&table).await.unwrap(), 1);

    let hits = driver
        .similar(&table, vec![1.0, 0.0, 0.0], 5)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "keep");

    // Deleting an unknown id is a no-op, no error.
    driver
        .delete(&table, vec!["never_existed".to_string()])
        .await
        .unwrap();

    drop_table(&driver, &table).await;
}

#[tokio::test]
#[ignore]
async fn integration_metadata_roundtrips_object_and_null() {
    let url = match mariadb_url_or_skip("integration_metadata_roundtrips_object_and_null") {
        Some(u) => u,
        None => return,
    };
    let driver = MariaDbVectorDriver::from_url(&url).unwrap();
    let table = unique_table("md");
    drop_table(&driver, &table).await;

    let create_sql =
        MariaDbVectorDriver::ensure_table_sql(&table, 3, MariaDbDistance::Cosine).unwrap();
    sqlx::query(&create_sql)
        .execute(driver.pool())
        .await
        .expect("CREATE TABLE");

    let nested =
        serde_json::json!({"label": "alpha", "tags": ["a", "b"], "score": 0.42, "nested": {"k": "v"}});
    driver
        .upsert(
            &table,
            vec![
                VectorItem::new("with_md", vec![1.0, 0.0, 0.0], nested.clone()),
                VectorItem::new("null_md", vec![0.0, 1.0, 0.0], serde_json::Value::Null),
            ],
        )
        .await
        .unwrap();

    let hits = driver
        .similar(&table, vec![1.0, 0.0, 0.0], 1)
        .await
        .unwrap();
    assert_eq!(hits[0].id, "with_md");
    assert_eq!(hits[0].metadata, nested);

    let hits2 = driver
        .similar(&table, vec![0.0, 1.0, 0.0], 1)
        .await
        .unwrap();
    assert_eq!(hits2[0].id, "null_md");
    assert_eq!(hits2[0].metadata, serde_json::Value::Null);

    drop_table(&driver, &table).await;
}

#[tokio::test]
#[ignore]
async fn integration_vec_fromtext_accepts_our_format() {
    // Proves embedding_to_vec_text isn't just "looks right" but
    // actually parses through MariaDB's VEC_FROMTEXT for the cases
    // we care about (basic, scientific notation, negatives, large).
    let url = match mariadb_url_or_skip("integration_vec_fromtext_accepts_our_format") {
        Some(u) => u,
        None => return,
    };
    let driver = MariaDbVectorDriver::from_url(&url).unwrap();

    let cases: &[&[f32]] = &[
        &[1.0, 2.0, 3.0],
        &[0.0, 0.0, 0.0],
        &[-1.5, 0.25, -0.0001],
        &[1e-20, 1.5, 1e10],
        &[f32::MIN_POSITIVE, 1.0, f32::MAX],
    ];
    for v in cases {
        let text = MariaDbVectorDriver::embedding_to_vec_text(v).unwrap();
        // SELECT VEC_TOTEXT(VEC_FROMTEXT(?)) — if MariaDB can parse our
        // text into a vector, this returns the round-trip text.
        let row: Result<(String,), _> = sqlx::query_as("SELECT VEC_TOTEXT(VEC_FROMTEXT(?)) AS t")
            .bind(&text)
            .fetch_one(driver.pool())
            .await;
        let (out,) = row.unwrap_or_else(|e| panic!("VEC_FROMTEXT rejected {text:?}: {e}"));
        // Length should round-trip; values may shift slightly due to
        // float→f32 precision but the count must match.
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("VEC_TOTEXT JSON");
        let arr = parsed.as_array().expect("VEC_TOTEXT array");
        assert_eq!(
            arr.len(),
            v.len(),
            "length lost in VEC_FROMTEXT round-trip for {v:?}"
        );
    }
}

#[tokio::test]
#[ignore]
async fn integration_delete_chunks_across_multiple_batches() {
    // Insert more ids than the driver's internal DELETE_BATCH_SIZE
    // (1000 at time of writing) so a single delete() call has to
    // split into multiple statements wrapped in one transaction.
    // Verifies the chunking path actually runs against MariaDB and
    // produces a clean count == 0 at the end.
    let url = match mariadb_url_or_skip("integration_delete_chunks_across_multiple_batches") {
        Some(u) => u,
        None => return,
    };
    let driver = MariaDbVectorDriver::from_url(&url).unwrap();
    let table = unique_table("chunk_del");
    drop_table(&driver, &table).await;

    let create_sql =
        MariaDbVectorDriver::ensure_table_sql(&table, 3, MariaDbDistance::Cosine).unwrap();
    sqlx::query(&create_sql)
        .execute(driver.pool())
        .await
        .expect("CREATE TABLE");

    // 1500 items — comfortably above 1000, low enough that the test
    // stays under a couple seconds even with HNSW index updates.
    const N: usize = 1500;
    let items: Vec<VectorItem> = (0..N)
        .map(|i| {
            VectorItem::new(
                format!("id_{i:05}"),
                vec![i as f32, (i + 1) as f32, (i + 2) as f32],
                serde_json::Value::Null,
            )
        })
        .collect();
    let ids: Vec<String> = items.iter().map(|it| it.id.clone()).collect();

    driver
        .upsert(&table, items)
        .await
        .expect("upsert 1500 items");
    assert_eq!(driver.count(&table).await.unwrap(), N);

    driver
        .delete(&table, ids)
        .await
        .expect("delete 1500 ids in one call");
    assert_eq!(
        driver.count(&table).await.unwrap(),
        0,
        "all rows must be removed even when the IN-list spans batches"
    );

    drop_table(&driver, &table).await;
}

#[tokio::test]
#[ignore]
async fn integration_count_on_empty_table_is_zero() {
    let url = match mariadb_url_or_skip("integration_count_on_empty_table_is_zero") {
        Some(u) => u,
        None => return,
    };
    let driver = MariaDbVectorDriver::from_url(&url).unwrap();
    let table = unique_table("empty");
    drop_table(&driver, &table).await;

    let create_sql =
        MariaDbVectorDriver::ensure_table_sql(&table, 3, MariaDbDistance::Cosine).unwrap();
    sqlx::query(&create_sql)
        .execute(driver.pool())
        .await
        .expect("CREATE TABLE");

    assert_eq!(driver.count(&table).await.unwrap(), 0);
    drop_table(&driver, &table).await;
}
