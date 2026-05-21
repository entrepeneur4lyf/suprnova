//! Phase 10C T7 — Pagination on `Builder<M>`.
//!
//! Three paginator types:
//! - `LengthAwarePaginator<M>` — offset + COUNT(*); knows `total` and
//!   `last_page`.
//! - `Paginator<M>` — simple, no COUNT; checks an extra row for
//!   `has_more`.
//! - `CursorPaginator<M>` — opaque keyset over the PK; forward-only.
//!
//! Page parameter defaults to `"page"`; override via
//! `paginate_using("custom", per_page)`. Cursor parameter defaults to
//! `"cursor"`. All three Serialize to Laravel's JSON shape for direct
//! Inertia / API shipping.
//!
//! Tests install per-thread query-param overrides via
//! `Context::test_set_query` / `test_clear_query` — every test calls
//! `test_clear_query()` up front so the previous test's state (if any
//! leaked through Cargo's thread-pool reuse) is wiped.

use chrono::{DateTime, Utc};
use suprnova::context::Context;
use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, CursorPaginator, LengthAwarePaginator, Model, Paginator};

// ---- Fixture -----------------------------------------------------------

#[model(table = "t7_articles", fillable = ["title"])]
pub struct T7Article {
    pub id: i64,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE t7_articles (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            title TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .expect("create t7_articles");
}

async fn seed(n: usize) {
    for i in 0..n {
        T7Article::create(attrs! { title: format!("a{}", i) })
            .await
            .expect("seed t7_articles");
    }
}

async fn fixture(n: usize) -> TestDatabase {
    Context::test_clear_query();
    // Cursor paginate emits encrypted cursors via `CursorPaginator::encode_value`,
    // which requires Crypt to be initialised. Test binaries don't run
    // `Server::from_config`, so we install a deterministic test key
    // ourselves. Idempotent — the first installer in the binary wins;
    // subsequent calls are no-ops.
    suprnova::testing::install_test_encryption_key();
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    seed(n).await;
    db
}

// ---- LengthAware paginate ----------------------------------------------

#[tokio::test]
async fn length_aware_paginate_includes_total() {
    let _db = fixture(25).await;
    let page: LengthAwarePaginator<T7Article> =
        T7Article::query().paginate(10).await.unwrap();

    assert_eq!(page.data.len(), 10);
    assert_eq!(page.total, 25);
    assert_eq!(page.last_page, 3);
    assert_eq!(page.current_page, 1);
    assert_eq!(page.per_page, 10);
    assert_eq!(page.from, Some(1));
    assert_eq!(page.to, Some(10));
}

#[tokio::test]
async fn length_aware_paginate_respects_existing_where_and_order() {
    let _db = fixture(25).await;
    // Filter to half the rows, then paginate.
    let page = T7Article::query()
        .filter_op("id", ">", 10)
        .paginate(5)
        .await
        .unwrap();
    assert_eq!(page.total, 15);
    assert_eq!(page.last_page, 3);
    assert_eq!(page.data.len(), 5);
}

#[tokio::test]
async fn length_aware_paginate_zero_per_page_errors() {
    let _db = fixture(5).await;
    let err = T7Article::query().paginate(0).await.expect_err("err");
    assert_eq!(err.status_code(), 400);
}

#[tokio::test]
async fn length_aware_paginate_empty_table_yields_no_window() {
    let _db = fixture(0).await;
    let page: LengthAwarePaginator<T7Article> =
        T7Article::query().paginate(10).await.unwrap();
    assert_eq!(page.total, 0);
    assert_eq!(page.last_page, 0);
    assert_eq!(page.data.len(), 0);
    assert_eq!(page.from, None);
    assert_eq!(page.to, None);
}

#[tokio::test]
async fn length_aware_paginate_count_handles_group_by() {
    // Without subquery-wrap, `SELECT COUNT(*) ... GROUP BY x` would
    // return one row per group (each carrying its group's size).
    // `query_one` would read only the first row's count, producing
    // a silently-wrong `total`.
    //
    // The fix: when `group_by` is non-empty, wrap into
    // `SELECT COUNT(*) FROM (<inner with GROUP BY>) AS sub` so the
    // wrapper counts distinct grouped rows.
    //
    // Seed 25 rows; group by title (each title is unique, so 25
    // distinct groups). Without the fix, `total` would be 1 (count
    // of the first group). With the fix, `total` is 25.
    let _db = fixture(25).await;

    let page: LengthAwarePaginator<T7Article> = T7Article::query()
        .group_by("title")
        .paginate(10)
        .await
        .unwrap();

    assert_eq!(
        page.total, 25,
        "GROUP BY paginate must count distinct groups, not the first group's size"
    );
}

// ---- paginate_using ----------------------------------------------------

#[tokio::test]
async fn paginate_using_custom_param_reads_request_query() {
    let _db = fixture(25).await;
    Context::test_set_query("p", "2");

    let page: LengthAwarePaginator<T7Article> = T7Article::query()
        .paginate_using("p", 10)
        .await
        .unwrap();

    assert_eq!(page.current_page, 2);
    assert_eq!(page.data.len(), 10);
    assert_eq!(page.from, Some(11));
    assert_eq!(page.to, Some(20));

    Context::test_clear_query();
}

#[tokio::test]
async fn paginate_using_wires_page_name_into_url_for_page() {
    // url_for_page must reflect the custom page param. Without
    // `with_page_name`, calling `.url_for_page(2)` after
    // `paginate_using("posts_page", ...)` would emit `?page=2` — a
    // silent footgun.
    let _db = fixture(25).await;
    Context::test_set_query("posts_page", "1");

    let page = T7Article::query()
        .paginate_using("posts_page", 10)
        .await
        .unwrap();

    assert_eq!(
        page.with_path("/api/articles").url_for_page(2),
        "/api/articles?posts_page=2"
    );

    Context::test_clear_query();
}

#[tokio::test]
async fn paginate_using_does_not_react_to_default_page_param() {
    // The default `paginate` reads `?page` — `paginate_using("p", N)`
    // must NOT pick up `?page=2`, otherwise the override would be a
    // no-op in disguise.
    let _db = fixture(25).await;
    Context::test_set_query("page", "2");

    let page = T7Article::query()
        .paginate_using("p", 10)
        .await
        .unwrap();

    // `?p` is missing, so current_page falls back to 1 — proving the
    // custom param plumbing is wired correctly.
    assert_eq!(page.current_page, 1);

    Context::test_clear_query();
}

// ---- simple paginate ---------------------------------------------------

#[tokio::test]
async fn simple_paginate_returns_page_with_has_more() {
    let _db = fixture(25).await;
    let page: Paginator<T7Article> = T7Article::query().simple_paginate(10).await.unwrap();
    assert_eq!(page.data.len(), 10);
    assert_eq!(page.current_page, 1);
    assert_eq!(page.per_page, 10);
    assert!(page.has_more);
}

#[tokio::test]
async fn simple_paginate_last_page_has_no_more() {
    let _db = fixture(25).await;
    Context::test_set_query("page", "3");

    let page: Paginator<T7Article> = T7Article::query().simple_paginate(10).await.unwrap();

    assert_eq!(page.current_page, 3);
    assert_eq!(page.data.len(), 5); // last 5 of 25
    assert!(!page.has_more);

    Context::test_clear_query();
}

#[tokio::test]
async fn simple_paginate_zero_per_page_errors() {
    let _db = fixture(5).await;
    let err = T7Article::query()
        .simple_paginate(0)
        .await
        .expect_err("err");
    assert_eq!(err.status_code(), 400);
}

// ---- cursor paginate ---------------------------------------------------

#[tokio::test]
async fn cursor_paginate_threads_next_cursor() {
    let _db = fixture(25).await;
    Context::test_clear_query();

    let page1: CursorPaginator<T7Article> =
        T7Article::query().cursor_paginate(10).await.unwrap();
    assert_eq!(page1.data.len(), 10);
    assert_eq!(page1.per_page, 10);
    assert!(page1.next_cursor.is_some());
    assert!(page1.prev_cursor.is_none());

    let cursor = page1.next_cursor.clone().unwrap();
    Context::test_set_query("cursor", &cursor);

    let page2: CursorPaginator<T7Article> =
        T7Article::query().cursor_paginate(10).await.unwrap();
    assert_eq!(page2.data.len(), 10);
    assert!(page2.next_cursor.is_some());
    // Page 2 starts at the row strictly greater than page 1's last id.
    let last_p1 = page1.data.last().unwrap().id;
    let first_p2 = page2.data.first().unwrap().id;
    assert_eq!(first_p2, last_p1 + 1);

    Context::test_clear_query();
}

#[tokio::test]
async fn cursor_paginate_last_page_has_no_next_cursor() {
    let _db = fixture(15).await;
    Context::test_clear_query();

    // First page of 10 — 5 left.
    let page1 = T7Article::query().cursor_paginate(10).await.unwrap();
    assert!(page1.next_cursor.is_some());
    Context::test_set_query("cursor", page1.next_cursor.as_ref().unwrap());

    let page2: CursorPaginator<T7Article> =
        T7Article::query().cursor_paginate(10).await.unwrap();
    assert_eq!(page2.data.len(), 5);
    assert!(page2.next_cursor.is_none());

    Context::test_clear_query();
}

#[tokio::test]
async fn cursor_paginate_zero_per_page_errors() {
    let _db = fixture(5).await;
    let err = T7Article::query()
        .cursor_paginate(0)
        .await
        .expect_err("err");
    assert_eq!(err.status_code(), 400);
}

#[tokio::test]
async fn cursor_paginate_invalid_cursor_errors() {
    let _db = fixture(5).await;
    Context::test_set_query("cursor", "!!!not-base64!!!");
    let err = T7Article::query()
        .cursor_paginate(10)
        .await
        .expect_err("err");
    // ParamParse / Internal — either way, not 200.
    assert!(err.status_code() >= 400);
    Context::test_clear_query();
}

// ---- Serialize shape (Laravel parity) ----------------------------------

#[tokio::test]
async fn length_aware_serializes_to_laravel_shape() {
    let _db = fixture(25).await;
    let page: LengthAwarePaginator<T7Article> =
        T7Article::query().paginate(10).await.unwrap();

    let json = serde_json::to_value(&page).unwrap();
    let m = json.as_object().unwrap();

    assert!(m.contains_key("data"));
    assert!(m.contains_key("current_page"));
    assert!(m.contains_key("last_page"));
    assert!(m.contains_key("per_page"));
    assert!(m.contains_key("total"));
    assert!(m.contains_key("from"));
    assert!(m.contains_key("to"));
    // path is unset — skipped from the JSON.
    assert!(m.get("path").is_none());
}

#[tokio::test]
async fn simple_serializes_to_laravel_shape() {
    let _db = fixture(25).await;
    let page: Paginator<T7Article> = T7Article::query().simple_paginate(10).await.unwrap();

    let json = serde_json::to_value(&page).unwrap();
    let m = json.as_object().unwrap();

    assert!(m.contains_key("data"));
    assert!(m.contains_key("current_page"));
    assert!(m.contains_key("per_page"));
    assert!(m.contains_key("has_more"));
    assert!(m.get("path").is_none());
}

#[tokio::test]
async fn cursor_serializes_with_only_active_cursor_fields() {
    let _db = fixture(25).await;
    Context::test_clear_query();
    let page: CursorPaginator<T7Article> =
        T7Article::query().cursor_paginate(10).await.unwrap();

    let json = serde_json::to_value(&page).unwrap();
    let m = json.as_object().unwrap();

    assert!(m.contains_key("data"));
    assert!(m.contains_key("per_page"));
    assert!(m.contains_key("next_cursor"));
    assert!(m.contains_key("prev_cursor"));
    // prev_cursor is None — but still present as JSON null so client
    // schemas can rely on the field.
    assert!(m.get("prev_cursor").unwrap().is_null());
}
