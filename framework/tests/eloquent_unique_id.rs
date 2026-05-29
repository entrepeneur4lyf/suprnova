//! UUID / ULID primary-key generation via `#[model(unique_id = "...")]`.
//!
//! Suprnova's analogue of Laravel's `HasUuids` / `HasUlids` /
//! `HasVersion4Uuids` trait family. The macro auto-populates the PK
//! string column before INSERT when the caller didn't supply one.

use suprnova::eloquent::{HasUniqueId, UniqueIdKind};
use suprnova::testing::TestDatabase;
use suprnova::{Model, attrs, model};

#[model(
    table = "uid_users",
    primary_key = "id",
    key_type = "String",
    auto_increment = false,
    unique_id = "uuid"
)]
pub struct UidUser {
    pub id: String,
    pub name: String,
}

#[model(
    table = "uid_orders",
    primary_key = "id",
    key_type = "String",
    auto_increment = false,
    unique_id = "ulid"
)]
pub struct UidOrder {
    pub id: String,
    pub name: String,
}

#[model(
    table = "uid_v4_widgets",
    primary_key = "id",
    key_type = "String",
    auto_increment = false,
    unique_id = "uuid_v4"
)]
pub struct UidV4Widget {
    pub id: String,
    pub name: String,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared("CREATE TABLE uid_users (id TEXT PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();
    db.execute_unprepared("CREATE TABLE uid_orders (id TEXT PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();
    db.execute_unprepared("CREATE TABLE uid_v4_widgets (id TEXT PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();
}

#[tokio::test]
async fn uuid_v7_auto_populated_on_create() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = UidUser::create(attrs! { name: "Alice" }).await.unwrap();
    assert_eq!(u.id.len(), 36, "uuid v7 emits 36-char canonical form");
    assert!(
        UniqueIdKind::UuidV7.is_valid(&u.id),
        "{} not valid uuid",
        u.id
    );
    // Round-trip find.
    let found = UidUser::find(u.id.clone()).await.unwrap();
    assert!(found.is_some());
}

#[tokio::test]
async fn ulid_auto_populated_on_create() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let o = UidOrder::create(attrs! { name: "ord1" }).await.unwrap();
    assert_eq!(o.id.len(), 26, "ulid is 26 chars");
    assert!(
        o.id.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
        "ulid should be lowercase ascii"
    );
    assert!(UniqueIdKind::Ulid.is_valid(&o.id));
}

#[tokio::test]
async fn uuid_v4_auto_populated_on_create() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let w = UidV4Widget::create(attrs! { name: "w1" }).await.unwrap();
    assert!(UniqueIdKind::UuidV4.is_valid(&w.id));
}

#[tokio::test]
async fn caller_supplied_id_wins() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = UidUser::create(attrs! {
        id: "00000000-0000-0000-0000-000000000123",
        name: "Bob",
    })
    .await
    .unwrap();
    assert_eq!(u.id, "00000000-0000-0000-0000-000000000123");
}

#[tokio::test]
async fn has_unique_id_kind_const_is_correct() {
    assert_eq!(
        <UidUser as HasUniqueId>::UNIQUE_ID_KIND,
        UniqueIdKind::UuidV7
    );
    assert_eq!(
        <UidOrder as HasUniqueId>::UNIQUE_ID_KIND,
        UniqueIdKind::Ulid
    );
    assert_eq!(
        <UidV4Widget as HasUniqueId>::UNIQUE_ID_KIND,
        UniqueIdKind::UuidV4
    );
}
