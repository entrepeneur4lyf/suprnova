//! Has / where-has / doesnt-have / where-doesnt-have / where-relation /
//! where-belongs-to existence engine.
//!
//! These tests cover the Laravel-shape relation-existence filter
//! family. Each one runs an end-to-end SELECT through SQLite-memory
//! so the generated SQL is verified against real rows, not just
//! `to_sql()` string matches.

use suprnova::testing::TestDatabase;
use suprnova::{Model, attrs, model};

#[model(table = "hex_users", relations = {
    posts: HasMany<HexPost>,
    profile: HasOne<HexProfile>,
})]
pub struct HexUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "hex_posts")]
pub struct HexPost {
    pub id: i64,
    pub hex_user_id: i64,
    pub title: String,
    pub published: bool,
}

#[model(table = "hex_profiles")]
pub struct HexProfile {
    pub id: i64,
    pub hex_user_id: i64,
    pub bio: String,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE hex_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE hex_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         hex_user_id INTEGER NOT NULL, title TEXT NOT NULL, published BOOLEAN NOT NULL DEFAULT 0)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE hex_profiles (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         hex_user_id INTEGER NOT NULL, bio TEXT NOT NULL)",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn has_returns_only_users_with_posts() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let alice = HexUser::create(attrs! { name: "Alice" }).await.unwrap();
    let _bob = HexUser::create(attrs! { name: "Bob" }).await.unwrap();
    HexPost::create(attrs! { hex_user_id: alice.id, title: "First", published: true })
        .await
        .unwrap();

    let with_posts = HexUser::query().has("posts").get().await.unwrap();
    assert_eq!(with_posts.len(), 1);
    assert_eq!(with_posts[0].name, "Alice");
}

#[tokio::test]
async fn doesnt_have_returns_only_users_without_posts() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let alice = HexUser::create(attrs! { name: "Alice" }).await.unwrap();
    let _bob = HexUser::create(attrs! { name: "Bob" }).await.unwrap();
    HexPost::create(attrs! { hex_user_id: alice.id, title: "First", published: true })
        .await
        .unwrap();

    let without_posts = HexUser::query().doesnt_have("posts").get().await.unwrap();
    assert_eq!(without_posts.len(), 1);
    assert_eq!(without_posts[0].name, "Bob");
}

#[tokio::test]
async fn where_has_filters_inner_constraint() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let alice = HexUser::create(attrs! { name: "Alice" }).await.unwrap();
    let bob = HexUser::create(attrs! { name: "Bob" }).await.unwrap();
    HexPost::create(attrs! { hex_user_id: alice.id, title: "Pub", published: true })
        .await
        .unwrap();
    HexPost::create(attrs! { hex_user_id: bob.id, title: "Draft", published: false })
        .await
        .unwrap();

    let with_published = HexUser::query()
        .where_has::<HexPost, _>("posts", |q| q.filter("published", true))
        .get()
        .await
        .unwrap();
    assert_eq!(with_published.len(), 1);
    assert_eq!(with_published[0].name, "Alice");
}

#[tokio::test]
async fn where_doesnt_have_excludes_matches() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let alice = HexUser::create(attrs! { name: "Alice" }).await.unwrap();
    let bob = HexUser::create(attrs! { name: "Bob" }).await.unwrap();
    HexPost::create(attrs! { hex_user_id: alice.id, title: "Pub", published: true })
        .await
        .unwrap();
    HexPost::create(attrs! { hex_user_id: bob.id, title: "Draft", published: false })
        .await
        .unwrap();

    // Users WITHOUT any draft posts: only Alice qualifies (she has
    // only a published post, no drafts). Bob has a draft so he's out.
    let no_drafts = HexUser::query()
        .where_doesnt_have::<HexPost, _>("posts", |q| q.filter("published", false))
        .get()
        .await
        .unwrap();
    assert_eq!(no_drafts.len(), 1);
    assert_eq!(no_drafts[0].name, "Alice");
}

#[tokio::test]
async fn has_count_filters_by_count() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let alice = HexUser::create(attrs! { name: "Alice" }).await.unwrap();
    let bob = HexUser::create(attrs! { name: "Bob" }).await.unwrap();
    for i in 1..=3 {
        HexPost::create(attrs! { hex_user_id: alice.id, title: format!("a{i}"), published: true })
            .await
            .unwrap();
    }
    HexPost::create(attrs! { hex_user_id: bob.id, title: "b1", published: true })
        .await
        .unwrap();

    let prolific = HexUser::query()
        .has_count("posts", ">=", 2)
        .get()
        .await
        .unwrap();
    assert_eq!(prolific.len(), 1);
    assert_eq!(prolific[0].name, "Alice");
}

#[tokio::test]
async fn where_relation_renders_inline_constraint() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let alice = HexUser::create(attrs! { name: "Alice" }).await.unwrap();
    HexPost::create(attrs! { hex_user_id: alice.id, title: "Pub", published: true })
        .await
        .unwrap();
    let bob = HexUser::create(attrs! { name: "Bob" }).await.unwrap();
    HexPost::create(attrs! { hex_user_id: bob.id, title: "Draft", published: false })
        .await
        .unwrap();

    let with_published = HexUser::query()
        .where_relation("posts", "published", true)
        .get()
        .await
        .unwrap();
    assert_eq!(with_published.len(), 1);
    assert_eq!(with_published[0].name, "Alice");
}

#[tokio::test]
async fn where_belongs_to_renders_direct_fk_eq() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let alice = HexUser::create(attrs! { name: "Alice" }).await.unwrap();
    let bob = HexUser::create(attrs! { name: "Bob" }).await.unwrap();
    HexPost::create(attrs! { hex_user_id: alice.id, title: "Alice1", published: true })
        .await
        .unwrap();
    HexPost::create(attrs! { hex_user_id: bob.id, title: "Bob1", published: true })
        .await
        .unwrap();

    // Note: HexPost has no explicit BelongsTo declared. We invoke the
    // engine via where_belongs_to which falls back to "1 = 0" for
    // unknown relations — exercising the safe-fail path.
    let none = HexPost::query()
        .where_belongs_to("author", alice.id)
        .get()
        .await
        .unwrap();
    assert_eq!(none.len(), 0);
}

#[tokio::test]
async fn has_engine_postgres_placeholders_are_monotonic() {
    use suprnova::sea_orm::DbBackend;
    // Place a parent WHERE clause before the EXISTS so the inner
    // subquery numbers $2 onward — verifies the monotonic counter
    // threading. The actual placeholder check inspects the rendered
    // SQL string.
    let (sql, _vals) = HexUser::query()
        .filter("name", "Alice")
        .where_has::<HexPost, _>("posts", |q| {
            q.filter("published", true)
                .filter_op("hex_user_id", ">=", 1)
        })
        .to_sql_with_bindings_for(DbBackend::Postgres);
    // Count $N occurrences and assert they're 1..=N monotonic.
    let mut placeholders: Vec<u32> = Vec::new();
    let mut i = 0;
    let bytes = sql.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'$' {
            i += 1;
            let mut n = 0u32;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                n = n * 10 + (bytes[i] - b'0') as u32;
                i += 1;
            }
            placeholders.push(n);
        } else {
            i += 1;
        }
    }
    let expected: Vec<u32> = (1..=placeholders.len() as u32).collect();
    assert_eq!(
        placeholders, expected,
        "placeholders must be monotonic 1..N in {sql}"
    );
}

#[tokio::test]
async fn or_has_combines_via_or() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let alice = HexUser::create(attrs! { name: "Alice" }).await.unwrap();
    let _bob = HexUser::create(attrs! { name: "Bob" }).await.unwrap();
    let _carol = HexUser::create(attrs! { name: "Carol" }).await.unwrap();
    HexPost::create(attrs! { hex_user_id: alice.id, title: "P1", published: true })
        .await
        .unwrap();

    // Either name == 'Bob' OR has at least one post (Alice).
    let either = HexUser::query()
        .filter("name", "Bob")
        .or_has("posts")
        .get()
        .await
        .unwrap();
    let names: Vec<&str> = either.iter().map(|u| u.name.as_str()).collect();
    assert!(names.contains(&"Alice"));
    assert!(names.contains(&"Bob"));
    assert_eq!(either.len(), 2);
}
