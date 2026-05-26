//! Phase 10C T8 — Chunking and lazy iteration on `Builder<M>`.
//!
//! Seven streaming entry points: `chunk` (OFFSET batches),
//! `chunk_by_id` (PK-cursor batches, concurrent-safe), `chunk_map`
//! (chunk + per-chunk map), `each` (single-row closure), `lazy`
//! (`LazyCollection<M>` stream, internally batched), `lazy_by_id`
//! (custom batch size), `cursor` (Laravel alias for `lazy`).
//!
//! All seven memory-bound by their respective batch size and reject
//! eager loads up front — chunking-with-`.with(...)` would silently
//! drop the eager plan during the cross-batch builder clone, so we
//! surface it as a loud error instead.

use chrono::{DateTime, Utc};
use suprnova::testing::TestDatabase;
use suprnova::{Collection, Model, attrs, model};

// ---- Fixture -----------------------------------------------------------

#[model(table = "t8_orders", fillable = ["amount"])]
pub struct T8Order {
    pub id: i64,
    pub amount: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE t8_orders (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            amount INTEGER NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .expect("create t8_orders");
}

async fn seed(n: u64) {
    for i in 0..n {
        T8Order::create(attrs! { amount: (i as i64) * 10 })
            .await
            .expect("seed t8_orders");
    }
}

async fn fixture(rows: u64) -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    seed(rows).await;
    db
}

// ---- chunk -------------------------------------------------------------

#[tokio::test]
async fn chunk_iterates_all_rows_in_batches() {
    // 25 rows / 10 per chunk = [10, 10, 5]. Pins the OFFSET-paginated
    // shape against under- and over-shoot — the last batch is short,
    // the loop must terminate exactly once the partial batch lands.
    let _db = fixture(25).await;

    let mut batch_count = 0_usize;
    let mut row_count = 0_usize;
    let mut batch_sizes: Vec<usize> = Vec::new();

    T8Order::query()
        .chunk(10, |batch: Collection<T8Order>| {
            let size = batch.len();
            batch_count += 1;
            row_count += size;
            batch_sizes.push(size);
            async move { Ok(()) }
        })
        .await
        .expect("chunk");

    assert_eq!(batch_count, 3);
    assert_eq!(row_count, 25);
    assert_eq!(batch_sizes, vec![10, 10, 5]);
}

#[tokio::test]
async fn chunk_empty_table_yields_no_batches() {
    // No rows ⇒ no batches; the closure must not run.
    let _db = fixture(0).await;
    let mut ran = false;
    T8Order::query()
        .chunk(10, |_batch: Collection<T8Order>| {
            ran = true;
            async move { Ok(()) }
        })
        .await
        .expect("chunk");
    assert!(!ran, "closure must not run on empty result set");
}

#[tokio::test]
async fn chunk_exact_multiple_terminates_after_last_full_batch() {
    // 20 / 10 = [10, 10] then a third query returning zero rows
    // terminates the loop. Pins the empty-batch break path against
    // edge case where the partial-batch break doesn't trigger.
    let _db = fixture(20).await;
    let mut batches = 0_usize;
    T8Order::query()
        .chunk(10, |_batch: Collection<T8Order>| {
            batches += 1;
            async move { Ok(()) }
        })
        .await
        .expect("chunk");
    assert_eq!(batches, 2);
}

// ---- chunk_by_id -------------------------------------------------------

#[tokio::test]
async fn chunk_by_id_safe_under_concurrent_inserts() {
    // PK-cursor pagination is immune to concurrent inserts. Each
    // batch filters on `id > last_id`, so rows inserted mid-loop
    // with PKs above the current cursor land in later batches (and
    // never duplicate or skip an original row).
    //
    // This is the canonical pattern for production-grade bulk
    // processing — OFFSET-based `chunk` skips/duplicates rows when
    // the table shifts mid-iteration.
    let _db = fixture(20).await;

    let mut seen_ids: Vec<i64> = Vec::new();

    T8Order::query()
        .chunk_by_id(5, |batch: Collection<T8Order>| {
            let ids: Vec<i64> = batch.iter().map(|o| o.id).collect();
            seen_ids.extend(ids);
            async move {
                // Simulate a concurrent insert mid-iteration. The
                // inserted row's PK is above any seen so far; it
                // either lands in a later batch or terminates the
                // walk depending on timing — but never causes an
                // original row to skip or duplicate.
                T8Order::create(attrs! { amount: 99999_i64 })
                    .await
                    .expect("concurrent insert");
                Ok(())
            }
        })
        .await
        .expect("chunk_by_id");

    // Every original row (ids 1..=20) must appear exactly once.
    // Later batches may also include the concurrently-inserted
    // rows, so we slice the first 20 IDs and check coverage.
    let original: Vec<i64> = seen_ids.iter().copied().take(20).collect();
    assert_eq!(original, (1..=20).collect::<Vec<_>>());
}

#[tokio::test]
async fn chunk_by_id_short_last_batch_terminates() {
    // 12 rows / 5 per chunk: [5, 5, 2] — last batch is short, the
    // `count < n` branch must terminate cleanly.
    let _db = fixture(12).await;
    let mut sizes = Vec::new();
    T8Order::query()
        .chunk_by_id(5, |batch: Collection<T8Order>| {
            sizes.push(batch.len());
            async move { Ok(()) }
        })
        .await
        .expect("chunk_by_id");
    assert_eq!(sizes, vec![5, 5, 2]);
}

// ---- chunk_map ---------------------------------------------------------

#[tokio::test]
async fn chunk_map_concatenates_per_chunk_results() {
    // 12 rows / 5 per chunk = 3 batches. Per-batch totals:
    //   batch 1 (amounts 0,10,20,30,40):  0+10+20+30+40 = 100
    //   batch 2 (amounts 50,60,70,80,90): 50+60+70+80+90 = 350
    //   batch 3 (amounts 100,110):        100+110 = 210
    let _db = fixture(12).await;

    let summaries: Collection<i64> = T8Order::query()
        .chunk_map(5, |batch: Collection<T8Order>| async move {
            let total: i64 = batch.iter().map(|o| o.amount).sum();
            Ok(Collection::from_vec(vec![total]))
        })
        .await
        .expect("chunk_map");

    assert_eq!(summaries.into_vec(), vec![100, 350, 210]);
}

#[tokio::test]
async fn chunk_map_empty_table_yields_empty_collection() {
    let _db = fixture(0).await;
    let out: Collection<i64> = T8Order::query()
        .chunk_map(5, |batch: Collection<T8Order>| async move {
            let total: i64 = batch.iter().map(|o| o.amount).sum();
            Ok(Collection::from_vec(vec![total]))
        })
        .await
        .expect("chunk_map");
    assert!(out.is_empty());
}

// ---- each --------------------------------------------------------------

#[tokio::test]
async fn each_processes_one_row_at_a_time() {
    // 7 rows: amounts 0, 10, 20, 30, 40, 50, 60.
    let _db = fixture(7).await;

    let mut amounts: Vec<i64> = Vec::new();
    T8Order::query()
        .each(|row: T8Order| {
            amounts.push(row.amount);
            async move { Ok(()) }
        })
        .await
        .expect("each");

    assert_eq!(amounts, vec![0, 10, 20, 30, 40, 50, 60]);
}

#[tokio::test]
async fn each_empty_table_invokes_no_closure() {
    let _db = fixture(0).await;
    let mut ran = false;
    T8Order::query()
        .each(|_row: T8Order| {
            ran = true;
            async move { Ok(()) }
        })
        .await
        .expect("each");
    assert!(!ran);
}

// ---- lazy / lazy_by_id / cursor ----------------------------------------

#[tokio::test]
async fn lazy_streams_rows_one_at_a_time() {
    // PK-ordered iteration over 7 rows with amounts 0,10,...60.
    let _db = fixture(7).await;

    let mut stream = T8Order::query().lazy();
    let mut amounts: Vec<i64> = Vec::new();

    while let Some(item) = stream.next().await {
        let order = item.expect("lazy row");
        amounts.push(order.amount);
    }

    assert_eq!(amounts, vec![0, 10, 20, 30, 40, 50, 60]);
}

#[tokio::test]
async fn lazy_by_id_respects_custom_batch_size() {
    // Custom batch size doesn't change the consumer-visible stream
    // shape — same rows in the same order. The contract is that
    // internal round-trip count changes; user code can't observe
    // it directly except by row order.
    let _db = fixture(7).await;

    let mut stream = T8Order::query().lazy_by_id(3);
    let mut amounts: Vec<i64> = Vec::new();
    while let Some(item) = stream.next().await {
        let order = item.expect("lazy_by_id row");
        amounts.push(order.amount);
    }

    assert_eq!(amounts, vec![0, 10, 20, 30, 40, 50, 60]);
}

#[tokio::test]
async fn cursor_is_alias_for_lazy() {
    // `cursor` is the Laravel name; the behaviour must match
    // `lazy()` byte-for-byte. Pin against accidental divergence
    // (different default batch size, different terminal shape).
    let _db = fixture(3).await;

    let mut stream = T8Order::query().cursor();
    let mut amounts: Vec<i64> = Vec::new();
    while let Some(item) = stream.next().await {
        let order = item.expect("cursor row");
        amounts.push(order.amount);
    }

    assert_eq!(amounts, vec![0, 10, 20]);
}

#[tokio::test]
async fn lazy_empty_table_yields_no_rows() {
    let _db = fixture(0).await;
    let mut stream = T8Order::query().lazy();
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn lazy_respects_where_clauses() {
    // The Builder's WHERE state must thread through the cross-batch
    // clone. Without that, `lazy()` would silently return every row.
    let _db = fixture(10).await;

    let mut stream = T8Order::query().filter_op("amount", ">=", 50_i64).lazy();
    let mut amounts: Vec<i64> = Vec::new();
    while let Some(item) = stream.next().await {
        let order = item.expect("lazy row");
        amounts.push(order.amount);
    }

    // Rows 5,6,7,8,9 → amounts 50,60,70,80,90.
    assert_eq!(amounts, vec![50, 60, 70, 80, 90]);
}

// ---- eager-load rejection ----------------------------------------------

#[tokio::test]
async fn chunk_with_eager_load_returns_error() {
    // The chunking clone drops the type-erased eager plan, so we
    // reject `.with(...)` up front rather than silently iterating
    // without the eager loads. Users re-apply `.with(...)` inside
    // the per-chunk closure when needed.
    let _db = fixture(5).await;

    let err = T8Order::query()
        .with(["nonexistent"])
        .chunk(2, |_batch: Collection<T8Order>| async move { Ok(()) })
        .await
        .expect_err("chunk must reject eager loads");

    let msg = format!("{err}");
    assert!(
        msg.contains("eager loading"),
        "expected eager-loading error, got: {msg}"
    );
}

#[tokio::test]
async fn chunk_by_id_with_eager_load_returns_error() {
    // `chunk_by_id` shares the same per-batch builder.clone() loop as
    // `chunk` and the same eager-spec drop hazard. The rejection has
    // been in place since Phase 10C T8 but had no regression test —
    // surfaced by the AF3 audit, pinned here so the guard at
    // `framework/src/eloquent/builder.rs:2419-2423` doesn't silently
    // disappear in a future refactor.
    let _db = fixture(5).await;

    let err = T8Order::query()
        .with(["nonexistent"])
        .chunk_by_id(2, |_batch: Collection<T8Order>| async move { Ok(()) })
        .await
        .expect_err("chunk_by_id must reject eager loads");

    let msg = format!("{err}");
    assert!(
        msg.contains("eager loading"),
        "expected eager-loading error, got: {msg}"
    );
}

#[tokio::test]
async fn lazy_with_eager_load_yields_error() {
    // `lazy()` builds the stream lazily, so the eager-load
    // rejection lands as the first item the consumer pulls. That's
    // visibly louder than silently dropping the plan and worth
    // pinning against.
    let _db = fixture(5).await;

    let mut stream = T8Order::query().with(["nonexistent"]).lazy();
    let first = stream
        .next()
        .await
        .expect("stream yields at least one item");
    let err = first.expect_err("lazy must surface eager-load rejection");

    let msg = format!("{err}");
    assert!(
        msg.contains("eager loading"),
        "expected eager-loading error, got: {msg}"
    );
}
