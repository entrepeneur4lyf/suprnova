//! Phase 6A T7 — Factory + Seeder dogfood integration test.
//!
//! Stands up a fresh `sqlite::memory:` connection with the dogfood
//! app's migrations applied, registers the application's
//! `BaseSeeder`, runs `seed::run_all`, and verifies the expected row
//! counts land in `users` and `posts`.
//!
//! Exercises every Phase 6A surface end-to-end:
//!   - The Factory trait + FactoryBuilder (UserFactory, PostFactory)
//!   - The blanket Persistable impl for SeaORM ModelTrait
//!   - The Seeder trait + ordered registry (BaseSeeder)
//!   - `seed::run_all` orchestration via tracing::info per seeder
//!
//! `#[serial]` because we mutate three process globals: the seeder
//! registry, the test container's bound DB connection, and the
//! migration state.

use app::seeders::BaseSeeder;
use sea_orm::{Database, EntityTrait, PaginatorTrait};
use sea_orm_migration::MigratorTrait;
use serial_test::serial;
use suprnova::container::testing::TestContainer;
use suprnova::seed;
use suprnova::DbConnection;

#[tokio::test]
#[serial]
async fn base_seeder_creates_50_users_and_200_posts() {
    // Reset the seeder registry so a previous test's registrations
    // don't leak into this one.
    seed::clear();

    // Bind a fresh sqlite::memory: connection through the TestContainer
    // so any code calling `DB::connection()` — including the framework's
    // blanket `Persistable` impl — sees it.
    let _guard = TestContainer::fake();
    let conn = Database::connect("sqlite::memory:").await.unwrap();
    app::migrations::Migrator::up(&conn, None)
        .await
        .expect("migrations apply");
    let db_conn = DbConnection::from_raw(conn.clone());
    TestContainer::singleton(db_conn);

    // Register and run the dogfood seeder.
    seed::register::<BaseSeeder>();
    seed::run_all().await.expect("BaseSeeder runs to completion");

    // Verify the row counts.
    let user_count = app::models::users::Entity::find()
        .count(&conn)
        .await
        .unwrap();
    assert_eq!(
        user_count, 50,
        "UserFactory.count(50).create_many() produced 50 rows"
    );

    let post_count = app::models::posts::Entity::find()
        .count(&conn)
        .await
        .unwrap();
    assert_eq!(
        post_count, 200,
        "PostFactory.count(200).create_many() produced 200 rows"
    );

    // A post sample carries non-empty title/body — the fake
    // generators populated them.
    let sample = app::models::posts::Entity::find()
        .one(&conn)
        .await
        .unwrap()
        .expect("at least one post exists");
    assert!(
        !sample.title.is_empty() && sample.title.len() > 3,
        "title is a multi-word sentence: {:?}",
        sample.title
    );
    assert!(
        !sample.body.is_empty() && sample.body.len() > 20,
        "body is a paragraph: len={}",
        sample.body.len()
    );
    assert!(
        (1..=50).contains(&sample.author_id),
        "author_id points at a user in 1..=50: {}",
        sample.author_id
    );

    seed::clear();
}
