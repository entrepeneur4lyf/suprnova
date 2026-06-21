//! Regression for the batched nested eager loader.
//!
//! `with(["posts.comments"])` previously recursed the tail segment
//! per-parent — one comments query per user (N+1). The fix gathers every
//! parent's children into one slice, issues a single IN query for the
//! next segment, recurses once, then redistributes the children back to
//! their original parents.
//!
//! Two properties are pinned here:
//!
//! 1. **Query count** — `with(["posts.comments"])` issues exactly one
//!    SELECT per level (base + posts + comments = 3), never one query per
//!    parent (the N+1 the batched recurse exists to avoid). Eloquent
//!    reads now route through the instrumented `ExecutorChoice`
//!    terminals, so they surface in `DB::get_query_log()` and the count
//!    is directly observable.
//!
//! 2. **Correct redistribution** — each parent gets back EXACTLY its own
//!    children after the batch. The dataset is deliberately asymmetric
//!    (different child counts per parent, with content that encodes the
//!    owning parent), so any mis-ordering in the take/put-back would hand
//!    a parent another parent's rows and fail loudly.

use serial_test::serial;
use suprnova::testing::TestDatabase;
use suprnova::{DB, Model, attrs, model};

#[model(table = "egc_users", relations = {
    posts: HasMany<EgcPost>,
})]
pub struct EgcUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "egc_posts", relations = {
    comments: HasMany<EgcComment>,
})]
pub struct EgcPost {
    pub id: i64,
    pub egc_user_id: i64,
    pub title: String,
}

#[model(table = "egc_comments")]
pub struct EgcComment {
    pub id: i64,
    pub egc_post_id: i64,
    pub body: String,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE egc_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE egc_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         egc_user_id INTEGER NOT NULL, title TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE egc_comments (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         egc_post_id INTEGER NOT NULL, body TEXT NOT NULL)",
    )
    .await
    .unwrap();
}

#[tokio::test]
#[serial]
async fn batched_nested_eager_redistributes_children_to_correct_parents() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;

    // Asymmetric tree: post counts 0, 1, 2, 3 across four users, and each
    // post gets `post_index + 1` comments whose body encodes the owning
    // post. A put-back that mis-orders children would mis-attribute these.
    let post_counts = [0usize, 1, 2, 3];
    for (u, &pc) in post_counts.iter().enumerate() {
        let user = EgcUser::create(attrs! { name: format!("u{u}") })
            .await
            .unwrap();
        for p in 0..pc {
            let post = EgcPost::create(attrs! {
                egc_user_id: user.id,
                title: format!("u{u}-p{p}"),
            })
            .await
            .unwrap();
            for c in 0..=p {
                EgcComment::create(attrs! {
                    egc_post_id: post.id,
                    body: format!("u{u}-p{p}-c{c}"),
                })
                .await
                .unwrap();
            }
        }
    }

    let users = EgcUser::query()
        .order_by_asc("id")
        .with(["posts.comments"])
        .get()
        .await
        .unwrap();

    assert_eq!(users.len(), 4);
    for (u, user) in users.iter().enumerate() {
        let posts = user.posts_loaded();
        assert_eq!(
            posts.len(),
            post_counts[u],
            "user u{u} must own exactly its {} posts",
            post_counts[u],
        );
        for (p, post) in posts.iter().enumerate() {
            // Title proves the post belongs to this user.
            assert_eq!(post.title, format!("u{u}-p{p}"));
            let comments = post.comments_loaded();
            assert_eq!(
                comments.len(),
                p + 1,
                "post u{u}-p{p} must own exactly its {} comments",
                p + 1,
            );
            // Every comment body must encode THIS post — the proof that
            // the batched put-back returned each parent its own children.
            for comment in comments {
                assert!(
                    comment.body.starts_with(&format!("u{u}-p{p}-")),
                    "comment {:?} was mis-attributed to post u{u}-p{p}",
                    comment.body,
                );
            }
        }
    }
}

/// The nested eager load issues exactly one SELECT per level — never one
/// per parent. Now that Eloquent reads route through the instrumented
/// `ExecutorChoice` terminals, this is directly observable in the query
/// log; before, the count was only guaranteed "by construction".
#[tokio::test]
#[serial]
async fn nested_eager_load_issues_one_select_per_level() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;

    // Same asymmetric tree as the redistribution test: post counts
    // 0, 1, 2, 3 and `post_index + 1` comments per post.
    let post_counts = [0usize, 1, 2, 3];
    for (u, &pc) in post_counts.iter().enumerate() {
        let user = EgcUser::create(attrs! { name: format!("u{u}") })
            .await
            .unwrap();
        for p in 0..pc {
            let post = EgcPost::create(attrs! {
                egc_user_id: user.id,
                title: format!("u{u}-p{p}"),
            })
            .await
            .unwrap();
            for c in 0..=p {
                EgcComment::create(attrs! {
                    egc_post_id: post.id,
                    body: format!("u{u}-p{p}-c{c}"),
                })
                .await
                .unwrap();
            }
        }
    }

    // Enable + clear the log AFTER seeding so only the eager SELECTs
    // below are captured (the seeding INSERTs are excluded).
    DB::enable_query_log().unwrap();
    DB::flush_query_log().unwrap();

    let users = EgcUser::query()
        .order_by_asc("id")
        .with(["posts.comments"])
        .get()
        .await
        .unwrap();
    assert_eq!(users.len(), 4);

    let selects: Vec<String> = DB::get_query_log()
        .unwrap()
        .into_iter()
        .map(|q| q.sql)
        .filter(|sql| sql.trim_start().to_uppercase().starts_with("SELECT"))
        .collect();

    // Exactly three SELECTs: one base + one IN-query per nested level.
    // A regression to per-parent recursion on the nested segment would
    // push the comments count from 1 to one-per-post.
    assert_eq!(
        selects.len(),
        3,
        "expected 3 SELECTs (users, posts, comments); got {}:\n{}",
        selects.len(),
        selects.join("\n"),
    );
    assert_eq!(
        selects.iter().filter(|s| s.contains("egc_users")).count(),
        1,
        "one base SELECT against egc_users",
    );
    assert_eq!(
        selects.iter().filter(|s| s.contains("egc_posts")).count(),
        1,
        "one IN-query against egc_posts",
    );
    assert_eq!(
        selects
            .iter()
            .filter(|s| s.contains("egc_comments"))
            .count(),
        1,
        "one IN-query against egc_comments",
    );

    DB::flush_query_log().unwrap();
    DB::disable_query_log().unwrap();
}
