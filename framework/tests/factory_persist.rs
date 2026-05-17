//! Persistable + Factory `create` / `create_many` integration test.
//!
//! Stands up a real `sqlite::memory:` connection through SeaORM,
//! registers it via `TestContainer` so `DB::connection()` finds it,
//! and exercises the blanket `Persistable for ModelTrait` path:
//! `UserFactory::new().create()` must insert a row and return the
//! post-insert model (auto-incremented id resolved).
//!
//! `#[serial]` because `TestContainer::fake()`'s guard is process-wide
//! — concurrent tests would clobber each other's bound connection.

use fake::{Fake, Faker};
use sea_orm::{
    ConnectionTrait, Database, DbBackend, EntityTrait, Schema,
};
use serial_test::serial;
use suprnova::container::testing::TestContainer;
use suprnova::factory::{persist_via_seaorm, Factory};
use suprnova::DbConnection;

// Toy SeaORM entity used as the persist target. Mirrors the
// `framework/tests/pagination.rs` pattern.
mod toy_user {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "toy_users")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub name: String,
        pub email: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}
}

impl fake::Dummy<Faker> for toy_user::Model {
    fn dummy_with_rng<R: rand::Rng + ?Sized>(_: &Faker, rng: &mut R) -> Self {
        let name: String = fake::faker::name::en::Name().fake_with_rng(rng);
        let email: String = fake::faker::internet::en::SafeEmail().fake_with_rng(rng);
        Self {
            // `0` triggers SQLite's auto-increment via SeaORM's
            // `NotSet` path; the post-insert model carries the real id.
            id: 0,
            name,
            email,
        }
    }
}

struct UserFactory;
impl Factory for UserFactory {
    type Model = toy_user::Model;
    fn definition() -> toy_user::Model {
        Faker.fake::<toy_user::Model>()
    }
}

/// Wire up `DB::connection()` to a fresh sqlite::memory: connection
/// with the `toy_users` table created. Returns the
/// `TestContainerGuard` keepalive — DO NOT drop it before the test
/// completes, or `DB::connection()` will lose the binding mid-test.
async fn fresh_db() -> (
    suprnova::container::testing::TestContainerGuard,
    sea_orm::DatabaseConnection,
) {
    let guard = TestContainer::fake();
    let conn = Database::connect("sqlite::memory:").await.unwrap();

    // Build the toy_users table from the SeaORM entity definition.
    let schema = Schema::new(DbBackend::Sqlite);
    let stmt = schema.create_table_from_entity(toy_user::Entity);
    conn.execute(conn.get_database_backend().build(&stmt))
        .await
        .unwrap();

    // Bind the connection so `DB::connection()` (and therefore the
    // blanket `Persistable` impl on every SeaORM model) finds it.
    let db_conn = DbConnection::from_raw(conn.clone());
    TestContainer::singleton(db_conn);

    (guard, conn)
}

#[tokio::test]
#[serial]
async fn factory_create_persists_through_db_connection_blanket_impl() {
    let (_guard, conn) = fresh_db().await;

    let user = UserFactory::new().create().await.expect("insert succeeds");

    // SeaORM assigns the auto-incremented id; the post-insert model
    // must carry the real id, NOT the 0 we wrote in the Dummy impl.
    assert!(
        user.id > 0,
        "post-insert id is auto-assigned: got {}",
        user.id
    );
    assert!(!user.name.is_empty());
    assert!(user.email.contains('@'));

    // The row really did land in the DB.
    let row_count = toy_user::Entity::find()
        .all(&conn)
        .await
        .unwrap()
        .len();
    assert_eq!(row_count, 1, "exactly one row in toy_users");
}

#[tokio::test]
#[serial]
async fn factory_create_many_persists_n_rows() {
    let (_guard, conn) = fresh_db().await;

    let users = UserFactory::new()
        .count(7)
        .create_many()
        .await
        .expect("create_many succeeds");

    assert_eq!(users.len(), 7);
    assert!(users.iter().all(|u| u.id > 0), "every row got an id");
    // Ids are unique (SQLite AUTOINCREMENT semantics).
    let ids: std::collections::HashSet<_> = users.iter().map(|u| u.id).collect();
    assert_eq!(ids.len(), 7, "every persisted row has a distinct id");

    let row_count = toy_user::Entity::find()
        .all(&conn)
        .await
        .unwrap()
        .len();
    assert_eq!(row_count, 7);
}

#[tokio::test]
#[serial]
async fn factory_create_with_override_persists_overridden_field() {
    let (_guard, conn) = fresh_db().await;

    let user = UserFactory::new()
        .with(|u| u.name = "Alice From Override".into())
        .create()
        .await
        .unwrap();
    assert_eq!(user.name, "Alice From Override");

    // Confirm the override hit the DB, not just the in-memory pre-insert.
    let row = toy_user::Entity::find_by_id(user.id)
        .one(&conn)
        .await
        .unwrap()
        .expect("row exists by id");
    assert_eq!(row.name, "Alice From Override");
}

/// Explicit-connection variant — bypass `DB::connection()` entirely
/// and persist against a connection the caller controls. Useful for
/// integration tests that need to coordinate two distinct databases
/// or for non-framework consumers.
#[tokio::test]
#[serial]
async fn persist_via_seaorm_helper_persists_against_explicit_connection() {
    let conn = Database::connect("sqlite::memory:").await.unwrap();
    let schema = Schema::new(DbBackend::Sqlite);
    let stmt = schema.create_table_from_entity(toy_user::Entity);
    conn.execute(conn.get_database_backend().build(&stmt))
        .await
        .unwrap();

    let m = UserFactory::new().make();
    let inserted = persist_via_seaorm(m, &conn).await.unwrap();
    assert!(inserted.id > 0);

    let row_count = toy_user::Entity::find()
        .all(&conn)
        .await
        .unwrap()
        .len();
    assert_eq!(row_count, 1);
}
