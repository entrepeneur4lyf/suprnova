//! Phase 10A T11 polish — auto-inject `AsDateTime` /
//! `AsOptionalDateTime` for every `DateTime<Utc>` / `Option<DateTime<Utc>>`
//! field on a `#[suprnova::model]` struct.
//!
//! ## Why this is needed
//!
//! SeaORM 1.1's `DeriveEntityModel` macro re-parses field types inside
//! `use sea_orm::entity::prelude::*` scope, which shadows
//! `chrono::DateTime` with SeaORM's `NaiveDateTime` alias. So a bare
//! `pub last_seen_at: DateTime<Utc>` on a Suprnova model would
//! mis-compile inside the inner `Model` declaration — the storage
//! shape would be `NaiveDateTime` while the user reads `DateTime<Utc>`.
//!
//! Before this polish landed, T9 / T10 worked around the issue ONLY
//! for the framework-managed columns (`created_at`, `updated_at`,
//! `deleted_at`). Any user-defined `DateTime<Utc>` column required a
//! manual `casts = { last_seen_at = AsDateTime }` declaration. This
//! generalisation auto-injects the right cast on every datetime
//! field unless the user already declared one — closing the gap so
//! `#[suprnova::model]` "just works" for natural timestamp shapes.
//!
//! User-declared casts on the same field still win — the auto-inject
//! is a fallback, not an override.

use chrono::{DateTime, Utc};
use suprnova::testing::TestDatabase;
use suprnova::{Model, attrs, model};

// `last_seen_at: DateTime<Utc>` is NOT one of the framework-managed
// timestamp columns; the auto-inject only fires because the field's
// type is `DateTime<Utc>`. `seen_count_at: Option<DateTime<Utc>>`
// exercises the optional branch (auto-inject = `AsOptionalDateTime`).
#[model(table = "auto_dt_users", fillable = ["name", "last_seen_at", "seen_count_at"])]
pub struct AutoDtUser {
    pub id: i64,
    pub name: String,
    pub last_seen_at: DateTime<Utc>,
    pub seen_count_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn migrate_auto_dt_users(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE auto_dt_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT NOT NULL, \
            last_seen_at TEXT NOT NULL, \
            seen_count_at TEXT, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn datetime_field_compiles_without_explicit_cast() {
    // Bare existence — if the macro didn't auto-inject `AsDateTime`,
    // this file wouldn't compile (SeaORM's DeriveEntityModel would
    // resolve `DateTime<Utc>` to `NaiveDateTime` and the
    // From<inner::Model> bridge would mismatch). The test reaching
    // runtime is itself the assertion.
    let _ = std::mem::size_of::<AutoDtUser>();
}

#[tokio::test]
async fn datetime_field_round_trips_through_create_and_find() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_auto_dt_users(&db).await;

    let when = Utc::now();
    let u = AutoDtUser::create(attrs! {
        name: "Alice",
        last_seen_at: when,
        seen_count_at: when,
    })
    .await
    .unwrap();

    // Round-trip: the AsDateTime auto-inject converted DateTime<Utc>
    // to its RFC-3339 string storage and back again.
    assert_eq!(u.last_seen_at.timestamp(), when.timestamp());
    assert_eq!(u.seen_count_at.unwrap().timestamp(), when.timestamp());

    let reread = AutoDtUser::find(u.id).await.unwrap().unwrap();
    assert_eq!(reread.last_seen_at.timestamp(), when.timestamp());
    assert_eq!(reread.seen_count_at.unwrap().timestamp(), when.timestamp());
}

#[tokio::test]
async fn optional_datetime_field_null_round_trips() {
    // The `AsOptionalDateTime` auto-inject must handle `None`
    // correctly: write NULL, read NULL back.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_auto_dt_users(&db).await;

    let u = AutoDtUser::create(attrs! {
        name: "Alice",
        last_seen_at: Utc::now(),
        // seen_count_at omitted — null in storage, None in runtime.
    })
    .await
    .unwrap();

    assert!(u.seen_count_at.is_none(), "omitted optional → None");

    let reread = AutoDtUser::find(u.id).await.unwrap().unwrap();
    assert!(reread.seen_count_at.is_none(), "NULL round-trips to None");
}
