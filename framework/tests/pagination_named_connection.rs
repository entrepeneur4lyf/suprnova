//! Facade named-connection routing — `Pagination::length_aware_on` /
//! `cursor_on`.
//!
//! This lives in its own test binary on purpose. Named-connection routing
//! reads the process-global `ConnectionRegistry`, and `TestContainer::fake()`'s
//! guard clears that registry on drop (teardown isolation). If this test ran
//! alongside others in a shared binary, a concurrently-finishing test's
//! guard-drop could wipe the registration between `register_existing` and the
//! `_on` lookup. As the sole test in this binary, nothing runs concurrently,
//! so the registry is stable for the test's duration.

use sea_orm::{ActiveModelTrait, ConnectionTrait, Database, DbBackend, EntityTrait, Schema, Set};
use suprnova::testing::TestContainer;
use suprnova::{ConnectionRegistry, DbConnection, EncryptionKey, Pagination};

fn ensure_crypt() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| suprnova::Crypt::init(EncryptionKey::generate()));
}

mod toy {
    use sea_orm::DeriveEntityModel;
    use sea_orm::entity::prelude::*;
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

async fn make_db_with_n_rows(n: i32) -> sea_orm::DatabaseConnection {
    let conn = Database::connect("sqlite::memory:").await.unwrap();
    let schema = Schema::new(DbBackend::Sqlite);
    let stmt = schema.create_table_from_entity(toy::Entity);
    conn.execute(conn.get_database_backend().build(&stmt))
        .await
        .unwrap();
    for i in 1..=n {
        toy::ActiveModel {
            id: Set(i),
            name: Set(format!("item-{i:02}")),
        }
        .insert(&conn)
        .await
        .unwrap();
    }
    conn
}

#[tokio::test]
async fn facade_on_routes_to_a_named_connection() {
    // The default DB holds 25 rows; a separate "reports" connection holds 7.
    // `length_aware_on` / `cursor_on` must route to "reports" (see 7) while
    // the plain methods use the default (see 25).
    ensure_crypt();
    let _guard = TestContainer::fake();
    TestContainer::singleton(DbConnection::from_raw(make_db_with_n_rows(25).await));

    ConnectionRegistry::register_existing(
        "reports",
        DbConnection::from_raw(make_db_with_n_rows(7).await),
    )
    .await
    .unwrap();

    let def = Pagination::length_aware::<toy::Entity>(toy::Entity::find(), 100, 1)
        .await
        .unwrap();
    assert_eq!(
        def.total, 25,
        "default facade must use the default connection"
    );

    let rep = Pagination::length_aware_on::<toy::Entity>("reports", toy::Entity::find(), 100, 1)
        .await
        .unwrap();
    assert_eq!(
        rep.total, 7,
        "length_aware_on must route to the named connection"
    );

    // A single 100-wide page over the 7-row "reports" connection holds all
    // 7 rows with no next cursor.
    let cur = Pagination::cursor_on::<toy::Entity, toy::Column>(
        "reports",
        toy::Entity::find(),
        None,
        100,
        toy::Column::Id,
    )
    .await
    .unwrap();
    assert_eq!(
        cur.data.len(),
        7,
        "cursor_on must route to the named connection"
    );
    assert!(cur.next_cursor.is_none());
}
