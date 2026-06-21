//! Regression for the batched nested eager loader.
//!
//! `with(["posts.comments"])` previously recursed the tail segment
//! per-parent — one comments query per user (N+1). The fix gathers every
//! parent's children into one slice, issues a single IN query for the
//! next segment, recurses once, then redistributes the children back to
//! their original parents.
//!
//! The query-count property (one IN query per nested level) is guaranteed
//! by construction: the batched recurse calls the child `eager_load`
//! exactly once per level. It can't be asserted via the query log here
//! because the Eloquent read path executes through SeaORM's `.all()`
//! rather than the instrumented `query_all`, so eager reads don't surface
//! in `DB::get_query_log()`.
//!
//! What this test DOES pin — and what the take/redistribute/put-back
//! implementation could actually get wrong — is that each parent gets
//! back EXACTLY its own children after the batch. The dataset is
//! deliberately asymmetric (different child counts per parent, with
//! content that encodes the owning parent), so any mis-ordering in the
//! put-back would hand a parent another parent's rows and fail loudly.

use serial_test::serial;
use suprnova::testing::TestDatabase;
use suprnova::{Model, attrs, model};

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
