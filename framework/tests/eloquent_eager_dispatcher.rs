//! Phase 10B T1 — verifies the macro-emitted dispatcher skeletons
//! (`__eager_load`, `__recurse_eager_load`, `__count_relation`,
//! `__aggregate_relation`) return clear "no relation" errors for
//! unknown names. T2-T7 add per-relation match arms.

use suprnova::model;
use suprnova::testing::TestDatabase;
use suprnova::AggregateKind;

#[model(table = "dispatcher_users", relations = {})]
pub struct DispatcherUser {
    pub id: i64,
    pub name: String,
}

#[tokio::test]
async fn unknown_relation_returns_internal_error() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE dispatcher_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
    )
    .await
    .unwrap();
    let mut u = DispatcherUser {
        id: 1,
        name: "A".into(),
        __eager: Default::default(),
        __pivot: None,
    };
    let mut parents = vec![&mut u];
    let err = DispatcherUser::__eager_load("nope", &mut parents, db.conn(), None)
        .await
        .expect_err("should fail for unknown relation");
    assert!(
        err.to_string().contains("no relation `nope`"),
        "expected `no relation `nope`` substring, got: {err}",
    );
}

#[tokio::test]
async fn unknown_count_relation_returns_error() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE dispatcher_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
    )
    .await
    .unwrap();
    let mut u = DispatcherUser {
        id: 1,
        name: "A".into(),
        __eager: Default::default(),
        __pivot: None,
    };
    let mut parents = vec![&mut u];
    let err = DispatcherUser::__count_relation("nope", &mut parents, db.conn())
        .await
        .expect_err("should fail for unknown relation");
    assert!(
        err.to_string().contains("no relation `nope`"),
        "expected `no relation `nope`` substring, got: {err}",
    );
}

#[tokio::test]
async fn unknown_aggregate_relation_returns_error() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE dispatcher_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
    )
    .await
    .unwrap();
    let mut u = DispatcherUser {
        id: 1,
        name: "A".into(),
        __eager: Default::default(),
        __pivot: None,
    };
    let mut parents = vec![&mut u];
    let err = DispatcherUser::__aggregate_relation(
        "nope",
        "amount",
        AggregateKind::Sum,
        &mut parents,
        db.conn(),
    )
    .await
    .expect_err("should fail for unknown relation");
    assert!(
        err.to_string().contains("no relation `nope`"),
        "expected `no relation `nope`` substring, got: {err}",
    );
}

#[tokio::test]
async fn unknown_recurse_relation_returns_error() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE dispatcher_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)",
    )
    .await
    .unwrap();
    let mut u = DispatcherUser {
        id: 1,
        name: "A".into(),
        __eager: Default::default(),
        __pivot: None,
    };
    let err = u
        .__recurse_eager_load("nope", "more.path", db.conn())
        .await
        .expect_err("should fail for unknown relation");
    assert!(
        err.to_string().contains("no relation `nope`"),
        "expected `no relation `nope`` substring, got: {err}",
    );
}

#[tokio::test]
async fn pivot_accessor_panics_without_context() {
    // T4 fills `__pivot` from the BelongsToMany loader; rows fetched
    // via `find()` or built via `default()` have None. Accessing
    // `pivot::<P>()` on such a row must panic with a clear message.
    //
    // `DispatcherUser` carries an `Arc<dyn Any + Send + Sync>` slot
    // for the pivot, which isn't `UnwindSafe` by default; wrap in
    // `AssertUnwindSafe` to bypass that — the panic is the test
    // surface, and we're not threading data across it.
    let u = DispatcherUser {
        id: 1,
        name: "A".into(),
        __eager: Default::default(),
        __pivot: None,
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: &i64 = u.pivot::<i64>();
    }));
    assert!(
        result.is_err(),
        "pivot::<P>() must panic when no pivot context is attached",
    );
}
