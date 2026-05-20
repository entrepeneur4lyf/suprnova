//! Phase 10A T11 — End-to-end coverage for the migrated app models
//! against the real `app::migrations::Migrator`.
//!
//! Uses `TestDatabase::fresh::<Migrator>()` — NOT `sqlite_memory()` +
//! manual DDL — so the test schema can never drift from the migrator
//! the dev DB actually runs. The same pattern Phase 11 / 13 app
//! dogfood tests use.

use app::migrations::Migrator;
use app::models::posts::Post;
use app::models::todos::Todo;
use app::models::users::User;
use suprnova::testing::TestDatabase;
use suprnova::{attrs, Model};

#[tokio::test]
async fn user_lifecycle_end_to_end() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();

    let alice = User::create(attrs! {
        name: "Alice",
        email: "alice@example.com",
        password: "hashed-by-app",
    })
    .await
    .unwrap();

    assert!(alice.active, "users default to active = true");
    assert!(alice.id > 0, "primary key was assigned by SQL");
    assert_eq!(alice.name, "Alice");

    let by_email = User::query()
        .filter("email", "alice@example.com")
        .first()
        .await
        .unwrap();
    assert!(by_email.is_some());

    // Cache the id before any moving call — soft delete / restore /
    // force_delete all take `self` (matching the Model trait signature
    // so the inherent overrides actually fire, see T10).
    let alice_id = alice.id;

    // Soft delete:
    alice.delete().await.unwrap();
    assert!(
        User::find(alice_id).await.unwrap().is_none(),
        "default scope hides trashed rows"
    );
    assert!(
        User::with_trashed()
            .filter("id", alice_id)
            .first()
            .await
            .unwrap()
            .is_some(),
        "with_trashed includes trashed rows"
    );

    // Restore + force delete:
    let trashed = User::with_trashed()
        .filter("id", alice_id)
        .first()
        .await
        .unwrap()
        .unwrap();
    trashed.restore().await.unwrap();
    let restored = User::find(alice_id).await.unwrap().unwrap();
    restored.force_delete().await.unwrap();
    assert!(
        User::with_trashed()
            .filter("id", alice_id)
            .first()
            .await
            .unwrap()
            .is_none(),
        "force_delete removes the row entirely"
    );
}

#[tokio::test]
async fn dual_api_users_consistent_sql() {
    // SQL emission is independent of any DB connection — verifies the
    // Laravel-named methods (`db_where`, `where_in`) and the
    // Rust-named methods (`filter`, `filter_in`) produce byte-identical
    // SQL.
    let rust_sql = User::query()
        .filter("active", true)
        .filter_in("email", ["a@x.com", "b@x.com"])
        .to_sql();
    let laravel_sql = User::query()
        .db_where("active", true)
        .where_in("email", ["a@x.com", "b@x.com"])
        .to_sql();
    assert_eq!(rust_sql, laravel_sql);
}

#[tokio::test]
async fn cast_round_trip_on_real_app_model() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();

    let t = Todo::create(attrs! { title: "Buy milk", done: false })
        .await
        .unwrap();
    assert!(!t.done, "AsBool cast deserialises 0 → false");

    let mut handle = t.clone();
    handle.done = true;
    handle.save().await.unwrap();

    let reread = Todo::find(t.id).await.unwrap().unwrap();
    assert!(reread.done, "AsBool round-trips true → 1 → true");
}

#[tokio::test]
async fn inventory_registry_contains_app_models() {
    let names: Vec<&'static str> = suprnova::models().map(|m| m.type_name).collect();
    assert!(
        names.contains(&"User"),
        "expected User in registry, got {names:?}"
    );
    assert!(
        names.contains(&"Post"),
        "expected Post in registry, got {names:?}"
    );
    assert!(
        names.contains(&"Todo"),
        "expected Todo in registry, got {names:?}"
    );
    // The framework's own Feature model should also be registered.
    assert!(
        names.contains(&"Feature"),
        "expected framework's Feature in registry, got {names:?}"
    );
}

#[tokio::test]
async fn hidden_fields_dropped_from_user_to_json() {
    let u = User {
        id: 1,
        name: "Alice".into(),
        email: "a@x.com".into(),
        password: "secret".into(),
        remember_token: Some("tok".into()),
        active: true,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
        ..Default::default()
    };
    let v = u.to_json();
    assert_eq!(v["name"], "Alice");
    assert!(v.get("password").is_none(), "password is hidden");
    assert!(
        v.get("remember_token").is_none(),
        "remember_token is hidden"
    );
}

#[tokio::test]
async fn fillable_filter_blocks_unlisted_user_columns() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();

    let user = User::create(attrs! {
        name: "Alice",
        email: "a@x.com",
        password: "hashed",
        // Not in `fillable = ["name", "email", "password"]` — should
        // fall through to the column default (active = true per the
        // migration).
        active: false,
        deleted_at: chrono::Utc::now(),
    })
    .await
    .unwrap();

    assert!(
        user.active,
        "active not in fillable — falls back to column default true"
    );
    assert!(
        user.deleted_at.is_none(),
        "deleted_at not in fillable — stays null"
    );
}

#[tokio::test]
async fn posts_via_eloquent_surface_round_trip() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();

    // Seed an author so the foreign-key column points somewhere
    // meaningful (the schema doesn't enforce FK but the Gate compares
    // post.author_id == user.id).
    let alice = User::create(attrs! {
        name: "Alice",
        email: "a@x.com",
        password: "pw",
    })
    .await
    .unwrap();

    let post = Post::create(attrs! {
        title: "Hello, world",
        body: "Body content.",
        is_public: true,
        author_id: alice.id,
    })
    .await
    .unwrap();
    assert!(post.id > 0);
    assert_eq!(post.author_id, alice.id);

    let fetched = Post::find_by_id(post.id).await.unwrap().unwrap();
    assert_eq!(fetched.title, "Hello, world");

    // `all_public` reuses the Eloquent query builder via the model's
    // `query()` entry point.
    let public = Post::all_public().await.unwrap();
    assert_eq!(public.len(), 1);
    assert_eq!(public[0].id, post.id);
}
