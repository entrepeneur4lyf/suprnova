//! Phase 10A T11 polish — `Persistable` covers Eloquent-facing
//! `#[suprnova::model]` structs, not just SeaORM `Model` rows.
//!
//! Before this polish landed, `Persistable` was blanket-implemented
//! for SeaORM `ModelTrait` only. A factory wanting to use Eloquent
//! types directly had to either:
//!
//! 1. Target the inner SeaORM `user::Model` storage shape (paying
//!    the storage-vs-runtime translation tax in `definition()`:
//!    `active: 1` instead of `active: true`, RFC-3339 strings for
//!    timestamps, etc.); or
//! 2. Reach for `User::create(attrs!{...})` and forfeit the typed
//!    `with(...)` override ergonomics factories provide.
//!
//! Neither was good. This test exercises the closing fix: the
//! `#[suprnova::model]` macro emits a per-struct `impl Persistable
//! for User { ... }` that converts the user-facing struct to its
//! inner SeaORM row, hands off to `persist_via_seaorm`, then maps
//! back. Factories now produce the runtime shape and the framework
//! bridges to storage transparently.

use chrono::{DateTime, Utc};
use suprnova::testing::TestDatabase;
use suprnova::{model, AsBool, Factory, Persistable};

// Eloquent-facing model with a runtime-shape `bool` field (cast to
// storage as INTEGER via `AsBool`) and runtime-shape `DateTime<Utc>`
// timestamp fields (cast to storage as TEXT via the auto-injected
// AsDateTime). The factory below produces runtime values; the
// emitted `Persistable for FactoryUser` impl bridges through the
// inner Model when persisting.
#[model(
    table = "factory_persist_users",
    fillable = ["name", "active"],
    casts = { active = AsBool },
)]
pub struct FactoryUser {
    pub id: i64,
    pub name: String,
    pub active: bool,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct FactoryUserFactory;

impl Factory for FactoryUserFactory {
    type Model = FactoryUser;

    fn definition() -> FactoryUser {
        // Runtime shape — exactly what a developer would write
        // by hand. No knowledge of storage-side INTEGER-for-bool or
        // RFC-3339-string-for-DateTime leaks into the factory.
        FactoryUser {
            // `0` is the placeholder — `persist_via_seaorm` flips
            // PK columns to `NotSet` so SQLite assigns the real id.
            id: 0,
            name: "Default Name".into(),
            active: true,
            last_seen_at: Some(Utc::now()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE factory_persist_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT NOT NULL, \
            active INTEGER NOT NULL, \
            last_seen_at TEXT, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn factory_targets_eloquent_struct_and_create_persists() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;

    let user = FactoryUserFactory::new()
        .with(|u| {
            u.name = "Alice".into();
            u.active = false;
        })
        .create()
        .await
        .unwrap();

    // PK assigned by SQLite.
    assert!(user.id > 0, "PK was assigned: {}", user.id);
    // Override took effect.
    assert_eq!(user.name, "Alice");
    // AsBool cast routed through correctly: runtime `false` → INTEGER
    // 0 → runtime `false`.
    assert!(!user.active);
}

#[tokio::test]
async fn factory_create_many_persists_multiple_eloquent_rows() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;

    let users = FactoryUserFactory::new()
        .count(3)
        .create_many()
        .await
        .unwrap();
    assert_eq!(users.len(), 3);
    for u in &users {
        assert!(u.id > 0, "PK assigned for each row");
        assert!(u.active, "default active = true survived round-trip");
    }

    // Confirm the rows landed in the database via the Eloquent
    // read path — closes the round-trip loop.
    let count = FactoryUser::count().await.unwrap();
    assert_eq!(count, 3);
}

#[tokio::test]
async fn direct_persistable_call_on_eloquent_struct_works() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;

    let user = FactoryUser {
        id: 0,
        name: "Direct".into(),
        active: true,
        last_seen_at: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    // Direct `.persist()` call — the per-struct impl is what makes
    // this compile without `as ::sea_orm::ModelTrait` shenanigans.
    let inserted = user.persist().await.unwrap();
    assert!(inserted.id > 0);
    assert_eq!(inserted.name, "Direct");
    assert!(inserted.last_seen_at.is_none());
}
