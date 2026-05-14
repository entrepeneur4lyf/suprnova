//! Integration tests for `LengthAwarePaginator`, `CursorPaginator`,
//! `Pagination::length_aware`/`cursor`, and the Inertia bridge.
//!
//! The cursor tests stand up a real in-memory SQLite database via the
//! `TestContainer` thread-local override so `Pagination::cursor` —
//! which uses `DB::connection()` internally — sees a connection.

use sea_orm::{
    ActiveModelTrait, ConnectionTrait, Database, DbBackend, EntityTrait, Schema, Set, Statement,
};
use serde::Serialize;
use serde_json::json;
use suprnova::testing::TestContainer;
use suprnova::{
    CursorPaginator, DbConnection, IntoInertiaScroll, LengthAwarePaginator, Pagination,
};

// Toy in-memory SQLite entity used by the integration tests.
mod toy {
    use sea_orm::entity::prelude::*;
    use sea_orm::DeriveEntityModel;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "items")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: i32,
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

// --- unit-level tests over the paginator types ---

#[test]
fn has_more_pages() {
    let p = LengthAwarePaginator::new(vec![1, 2, 3], 25, 10, 2);
    assert_eq!(p.last_page, 3);
    assert!(p.has_more_pages());
}

#[test]
fn last_page_no_more() {
    let p = LengthAwarePaginator::new(vec![1, 2, 3, 4, 5], 25, 10, 3);
    assert!(!p.has_more_pages());
}

#[test]
fn total_zero_yields_empty_data() {
    let p: LengthAwarePaginator<i32> = LengthAwarePaginator::new(vec![], 0, 10, 1);
    assert_eq!(p.last_page, 0);
    assert!(p.data.is_empty());
}

// --- SeaORM integration via in-memory SQLite ---

async fn make_db_with_n_rows(n: i32) -> sea_orm::DatabaseConnection {
    let conn = Database::connect("sqlite::memory:").await.unwrap();
    let schema = Schema::new(DbBackend::Sqlite);
    let stmt = schema.create_table_from_entity(toy::Entity);
    conn.execute(conn.get_database_backend().build(&stmt))
        .await
        .unwrap();

    for i in 1..=n {
        let m = toy::ActiveModel {
            id: Set(i),
            name: Set(format!("item-{:02}", i)),
        };
        m.insert(&conn).await.unwrap();
    }
    conn.execute(Statement::from_string(
        DbBackend::Sqlite,
        "SELECT 1".to_string(),
    ))
    .await
    .unwrap();
    conn
}

/// Mount a SeaORM connection on the thread-local test container so
/// `DB::connection()` resolves to it inside `Pagination::cursor`.
fn install_db(conn: sea_orm::DatabaseConnection) {
    let db = DbConnection::from_raw(conn);
    TestContainer::singleton(db);
}

#[tokio::test]
async fn seaorm_length_aware_page_2_returns_10_rows() {
    let _guard = TestContainer::fake();
    let conn = make_db_with_n_rows(25).await;
    install_db(conn.clone());

    let p = Pagination::length_aware::<toy::Entity>(toy::Entity::find(), 10, 2)
        .await
        .unwrap();
    assert_eq!(p.total, 25);
    assert_eq!(p.per_page, 10);
    assert_eq!(p.current_page, 2);
    assert_eq!(p.last_page, 3);
    assert_eq!(p.data.len(), 10);
    assert!(p.has_more_pages());
}

#[tokio::test]
async fn pagination_cursor_walks_forward_until_exhausted() {
    let _guard = TestContainer::fake();
    let conn = make_db_with_n_rows(25).await;
    install_db(conn);

    let per_page: u64 = 10;
    let mut visited: Vec<i32> = Vec::new();
    let mut cursor: Option<String> = None;

    for _ in 0..10 {
        let page = Pagination::cursor::<toy::Entity, toy::Column>(
            toy::Entity::find(),
            cursor.as_deref(),
            per_page,
            toy::Column::Id,
        )
        .await
        .unwrap();

        for r in &page.data {
            visited.push(r.id);
        }
        cursor = page.next_cursor.clone();
        if cursor.is_none() {
            break;
        }
    }

    assert_eq!(visited.len(), 25);
    assert_eq!(visited.first(), Some(&1));
    assert_eq!(visited.last(), Some(&25));
}

#[tokio::test]
async fn pagination_cursor_emits_prev_cursor_on_page_2() {
    let _guard = TestContainer::fake();
    let conn = make_db_with_n_rows(25).await;
    install_db(conn);

    // Page 1 — first page, prev_cursor must be None.
    let page1 = Pagination::cursor::<toy::Entity, toy::Column>(
        toy::Entity::find(),
        None,
        10,
        toy::Column::Id,
    )
    .await
    .unwrap();
    assert!(page1.prev_cursor.is_none(), "first page must have no prev_cursor");
    let next1 = page1.next_cursor.clone().expect("page 1 should have a next cursor");
    let page1_ids: Vec<i32> = page1.data.iter().map(|r| r.id).collect();
    assert_eq!(page1_ids, (1..=10).collect::<Vec<i32>>());

    // Page 2 — using page 1's next_cursor.
    let page2 = Pagination::cursor::<toy::Entity, toy::Column>(
        toy::Entity::find(),
        Some(&next1),
        10,
        toy::Column::Id,
    )
    .await
    .unwrap();
    let page2_ids: Vec<i32> = page2.data.iter().map(|r| r.id).collect();
    assert_eq!(page2_ids, (11..=20).collect::<Vec<i32>>());
    let prev2 = page2.prev_cursor.clone().expect("page 2 must emit a prev_cursor");

    // Following page 2's prev_cursor takes us back to page 1's rows.
    let back = Pagination::cursor::<toy::Entity, toy::Column>(
        toy::Entity::find(),
        Some(&prev2),
        10,
        toy::Column::Id,
    )
    .await
    .unwrap();
    let back_ids: Vec<i32> = back.data.iter().map(|r| r.id).collect();
    assert_eq!(
        back_ids,
        (1..=10).collect::<Vec<i32>>(),
        "prev_cursor from page 2 must return to page 1's contents"
    );
    // back has 10 rows and there are no more rows before id=1 → no prev.
    assert!(
        back.prev_cursor.is_none(),
        "walked back to the first page; prev_cursor should be None"
    );
    // We came from page 2, so we always have a way forward.
    assert!(back.next_cursor.is_some());
}

#[tokio::test]
async fn pagination_cursor_last_page_no_next() {
    let _guard = TestContainer::fake();
    let conn = make_db_with_n_rows(25).await;
    install_db(conn);

    let mut cursor: Option<String> = None;
    let mut last_page_rows: Vec<i32> = Vec::new();
    for _ in 0..10 {
        let p = Pagination::cursor::<toy::Entity, toy::Column>(
            toy::Entity::find(),
            cursor.as_deref(),
            10,
            toy::Column::Id,
        )
        .await
        .unwrap();
        last_page_rows = p.data.iter().map(|r| r.id).collect();
        if p.next_cursor.is_none() {
            // Last page reached. With 25 rows, this is page 3 (rows 21..=25).
            assert_eq!(last_page_rows, (21..=25).collect::<Vec<i32>>());
            return;
        }
        cursor = p.next_cursor;
    }
    panic!("walked too many pages; last page: {last_page_rows:?}");
}

// --- Live-DB tests (gated by #[ignore]) ---
//
// These exercise `Pagination::cursor` against real Postgres / MySQL,
// validating that the typed `sea_orm::Value` boundary binds correctly
// for native int4 / bigint / uuid columns on each dialect.
//
// They're skipped by default. To run them, set the URL env var (or
// rely on the localhost default) and pass `--ignored`:
//
//   PG_TEST_URL=postgres://postgres:postgres@localhost:5432/test \
//     cargo test -p suprnova --test pagination -- --ignored postgres
//
//   MYSQL_TEST_URL=mysql://root:root@localhost:3306/test \
//     cargo test -p suprnova --test pagination -- --ignored mysql
//
// The toy entity's `id` is `i32` (Int) on every dialect — so the
// cursor wire format roundtrips `Value::Int(Some(42))` through
// Postgres `int4`, MySQL `INT`, etc. without dialect-specific casts.

async fn try_connect_live(url: &str) -> Option<sea_orm::DatabaseConnection> {
    use sea_orm::ConnectOptions;
    use std::time::Duration;
    let mut opts = ConnectOptions::new(url.to_string());
    opts.connect_timeout(Duration::from_secs(2))
        .acquire_timeout(Duration::from_secs(2));
    sea_orm::Database::connect(opts).await.ok()
}

async fn populate_n(conn: &sea_orm::DatabaseConnection, n: i32) {
    // Drop the table if it lingers from a prior failed run; SeaORM's
    // `create_table_from_entity` doesn't issue IF NOT EXISTS.
    let _ = conn
        .execute(Statement::from_string(
            conn.get_database_backend(),
            "DROP TABLE IF EXISTS items".to_string(),
        ))
        .await;
    let schema = Schema::new(conn.get_database_backend());
    let stmt = schema.create_table_from_entity(toy::Entity);
    conn.execute(conn.get_database_backend().build(&stmt))
        .await
        .unwrap();
    for i in 1..=n {
        toy::ActiveModel {
            id: Set(i),
            name: Set(format!("item-{:02}", i)),
        }
        .insert(conn)
        .await
        .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires live Postgres; run with --ignored postgres"]
async fn live_postgres_cursor_walks_with_typed_int_boundary() {
    let url = std::env::var("PG_TEST_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/postgres".to_string());
    let conn = try_connect_live(&url)
        .await
        .expect("Postgres test DB not reachable — set PG_TEST_URL");
    populate_n(&conn, 25).await;

    let _guard = TestContainer::fake();
    install_db(conn);

    let mut visited: Vec<i32> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let p = Pagination::cursor::<toy::Entity, toy::Column>(
            toy::Entity::find(),
            cursor.as_deref(),
            10,
            toy::Column::Id,
        )
        .await
        .unwrap();
        for r in &p.data {
            visited.push(r.id);
        }
        cursor = p.next_cursor.clone();
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(visited.len(), 25);
    assert_eq!(visited.first(), Some(&1));
    assert_eq!(visited.last(), Some(&25));
}

#[tokio::test]
#[ignore = "requires live MySQL; run with --ignored mysql"]
async fn live_mysql_cursor_walks_with_typed_int_boundary() {
    let url = std::env::var("MYSQL_TEST_URL")
        .unwrap_or_else(|_| "mysql://root:root@localhost:3306/test".to_string());
    let conn = try_connect_live(&url)
        .await
        .expect("MySQL test DB not reachable — set MYSQL_TEST_URL");
    populate_n(&conn, 25).await;

    let _guard = TestContainer::fake();
    install_db(conn);

    let mut visited: Vec<i32> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let p = Pagination::cursor::<toy::Entity, toy::Column>(
            toy::Entity::find(),
            cursor.as_deref(),
            10,
            toy::Column::Id,
        )
        .await
        .unwrap();
        for r in &p.data {
            visited.push(r.id);
        }
        cursor = p.next_cursor.clone();
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(visited.len(), 25);
    assert_eq!(visited.first(), Some(&1));
    assert_eq!(visited.last(), Some(&25));
}

// --- IntoInertiaScroll wiring ---

#[test]
fn length_aware_into_inertia_scroll() {
    let p = LengthAwarePaginator::new(vec!["a", "b", "c"], 25, 10, 2);
    let (meta, data) = p.into_inertia_scroll();
    assert_eq!(meta.page_name, "page");
    assert_eq!(meta.current_page, Some(json!(2_i64)));
    assert_eq!(meta.previous_page, Some(json!(1_i64)));
    assert_eq!(meta.next_page, Some(json!(3_i64)));
    assert_eq!(data, vec!["a", "b", "c"]);
}

#[test]
fn cursor_into_inertia_scroll() {
    let p: CursorPaginator<String> = CursorPaginator {
        data: vec!["row-1".to_string(), "row-2".to_string()],
        next_cursor: Some("opaque-next".to_string()),
        prev_cursor: Some("opaque-prev".to_string()),
    };
    let (meta, data) = p.into_inertia_scroll();
    assert_eq!(meta.page_name, "cursor");
    assert_eq!(meta.next_page, Some(json!("opaque-next")));
    assert_eq!(meta.previous_page, Some(json!("opaque-prev")));
    assert_eq!(data.len(), 2);
}

#[test]
fn inertia_paginate_facade_produces_inertia_response() {
    #[derive(Serialize)]
    struct Row {
        id: i32,
    }
    let p = LengthAwarePaginator::new(vec![Row { id: 1 }, Row { id: 2 }], 2, 10, 1);
    // Just exercise the facade — we don't try to serialize the full
    // Inertia response here (that path runs through ScrollConfig
    // resolvers which need an InertiaContext / request).
    let _resp = suprnova::Inertia::paginate("Users/Index", "users", p);
}
