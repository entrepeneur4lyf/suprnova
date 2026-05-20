//! Phase 10B T6 — `MorphTo` / `MorphOne` / `MorphMany` + per-family
//! enum.
//!
//! The polymorphic primitives layer a `<name>_id` + `<name>_type`
//! column pair on the child table and let the parent-side `MorphMany`
//! / `MorphOne` filter children by both. The morph-table side declares
//! `MorphTo { name, targets = [...] }` and the macro emits a
//! per-family enum (`<Name>Morph`) with one variant per target plus
//! `Unknown(String, i64)` for legacy rows whose `<name>_type` column
//! doesn't match any registered target.
//!
//! Coverage matrix:
//!
//! - `morph_many_returns_comments_on_post` — basic parent-side query
//!   honours the morph-type filter.
//! - `morph_to_returns_correct_variant` — inverse-side dispatch lands
//!   in the right enum variant.
//! - `morph_to_returns_video_variant` — same, against the second
//!   declared target.
//! - `morph_to_unknown_for_unregistered_type` — legacy row with an
//!   unmatched `<name>_type` falls through to the `Unknown` variant.
//! - `morph_many_eager_load` — `Self::with(["comments"])` populates
//!   the per-row `__eager` cache (also confirms the morph-type
//!   predicate is on the eager-load query — see the comments-on-
//!   different-family assertion below).
//! - `morph_one_returns_single_morph` — `MorphOne` returns
//!   `Option<R>` from `.first()` with the same morph-type filter.
//! - `morph_many_count_uses_server_side_group_by` — count dispatcher
//!   reports per-parent fan-out without buffering child rows
//!   client-side.
//! - `morph_many_aggregate_via_server_side_group_by` — aggregate
//!   dispatcher applies SUM over the child column with the morph-type
//!   predicate honoured.
//! - `morph_many_count_filters_by_morph_type` — explicit assertion
//!   that count over Post.comments doesn't include comments on Video,
//!   even when Video has the same parent PK as Post.

use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, AggregateKind, Model};

// Comment.commentable is polymorphic — points at MorphPost OR
// MorphVideo. The relation lives on the morph-table side.
#[model(table = "morph_comments", relations = {
    commentable: MorphTo { name = "commentable", targets = [MorphPost, MorphVideo] },
})]
pub struct MorphComment {
    pub id: i64,
    pub commentable_id: i64,
    pub commentable_type: String,
    pub body: String,
}

#[model(table = "morph_posts", morph_type = "post", relations = {
    comments: MorphMany<MorphComment> { name = "commentable" },
    cover: MorphOne<MorphImage> { name = "imageable" },
})]
pub struct MorphPost {
    pub id: i64,
    pub title: String,
}

#[model(table = "morph_videos", morph_type = "video", relations = {
    comments: MorphMany<MorphComment> { name = "commentable" },
})]
pub struct MorphVideo {
    pub id: i64,
    pub url: String,
}

#[model(table = "morph_images")]
pub struct MorphImage {
    pub id: i64,
    pub imageable_id: i64,
    pub imageable_type: String,
    pub url: String,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE morph_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE morph_videos (id INTEGER PRIMARY KEY AUTOINCREMENT, url TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE morph_comments (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            commentable_id INTEGER NOT NULL, \
            commentable_type TEXT NOT NULL, \
            body TEXT NOT NULL, \
            views INTEGER NOT NULL DEFAULT 0\
         )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE morph_images (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            imageable_id INTEGER NOT NULL, \
            imageable_type TEXT NOT NULL, \
            url TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn morph_many_returns_comments_on_post() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MorphPost::create(attrs! { title: "p1" }).await.unwrap();
    let _ = MorphComment::create(attrs! {
        commentable_id: p.id,
        commentable_type: "post",
        body: "nice",
    })
    .await
    .unwrap();
    let _ = MorphComment::create(attrs! {
        commentable_id: p.id,
        commentable_type: "post",
        body: "agreed",
    })
    .await
    .unwrap();
    // A comment on a DIFFERENT video — must not appear.
    let v = MorphVideo::create(attrs! { url: "u.mp4" }).await.unwrap();
    let _ = MorphComment::create(attrs! {
        commentable_id: v.id,
        commentable_type: "video",
        body: "video comment",
    })
    .await
    .unwrap();

    let comments = p.comments().get().await.unwrap();
    assert_eq!(comments.len(), 2);
    assert!(comments.iter().any(|c| c.body == "nice"));
    assert!(comments.iter().any(|c| c.body == "agreed"));
    assert!(!comments.iter().any(|c| c.body.contains("video")));
}

#[tokio::test]
async fn morph_to_returns_correct_variant() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MorphPost::create(attrs! { title: "post-target" })
        .await
        .unwrap();
    let c = MorphComment::create(attrs! {
        commentable_id: p.id,
        commentable_type: "post",
        body: "x",
    })
    .await
    .unwrap();

    match c.commentable().get().await.unwrap() {
        CommentableMorph::MorphPost(parent) => {
            assert_eq!(parent.id, p.id);
            assert_eq!(parent.title, "post-target");
        }
        CommentableMorph::MorphVideo(_) => panic!("expected post variant"),
        CommentableMorph::Unknown(t, id) => {
            panic!("expected MorphPost variant, got Unknown({t}, {id})")
        }
    }
}

#[tokio::test]
async fn morph_to_unknown_for_unregistered_type() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    // Manually insert a comment pointing at a type not in the
    // morph_to targets list — simulates a legacy row or a renamed
    // model.
    _db.execute_unprepared(
        "INSERT INTO morph_comments (commentable_id, commentable_type, body) \
         VALUES (999, 'legacy_thing', 'unknown')",
    )
    .await
    .unwrap();
    let c = MorphComment::find(1)
        .await
        .unwrap()
        .expect("inserted comment");

    match c.commentable().get().await.unwrap() {
        CommentableMorph::Unknown(type_, id) => {
            assert_eq!(type_, "legacy_thing");
            assert_eq!(id, 999);
        }
        other => panic!("expected Unknown variant, got {other:?}"),
    }
}

#[tokio::test]
async fn morph_to_returns_video_variant() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let v = MorphVideo::create(attrs! { url: "abc.mp4" }).await.unwrap();
    let c = MorphComment::create(attrs! {
        commentable_id: v.id,
        commentable_type: "video",
        body: "vid",
    })
    .await
    .unwrap();

    match c.commentable().get().await.unwrap() {
        CommentableMorph::MorphVideo(parent) => {
            assert_eq!(parent.id, v.id);
            assert_eq!(parent.url, "abc.mp4");
        }
        _ => panic!("expected MorphVideo variant"),
    }
}

#[tokio::test]
async fn morph_to_returns_unknown_when_parent_row_missing() {
    // The morph-type IS registered (Post is in the targets list) but
    // the row at commentable_id has been deleted. The fetch helper
    // must surface `Unknown` rather than panicking, so callers can
    // recover or log the dangling FK.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    _db.execute_unprepared(
        "INSERT INTO morph_comments (commentable_id, commentable_type, body) \
         VALUES (99999, 'post', 'orphaned')",
    )
    .await
    .unwrap();
    let c = MorphComment::find(1)
        .await
        .unwrap()
        .expect("inserted comment");

    match c.commentable().get().await.unwrap() {
        CommentableMorph::Unknown(t, id) => {
            assert_eq!(t, "post");
            assert_eq!(id, 99999);
        }
        other => {
            panic!("expected Unknown for dangling FK, got {other:?}")
        }
    }
}

#[tokio::test]
async fn morph_one_returns_single_morph() {
    // MorphOne returns Option<R> from .first(). Parent's
    // morph_type = "post" controls the imageable_type predicate so an
    // image attached to a Video (different morph family) doesn't
    // surface here.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MorphPost::create(attrs! { title: "p" }).await.unwrap();
    let _ = MorphImage::create(attrs! {
        imageable_id: p.id,
        imageable_type: "post",
        url: "cover.jpg",
    })
    .await
    .unwrap();
    // A noise image with the SAME imageable_id but a different
    // morph_type — must not be returned.
    let _ = MorphImage::create(attrs! {
        imageable_id: p.id,
        imageable_type: "video",
        url: "noise.jpg",
    })
    .await
    .unwrap();

    let cover = p.cover().first().await.unwrap();
    assert!(cover.is_some(), "MorphOne::first must find the cover");
    let cover = cover.unwrap();
    assert_eq!(cover.url, "cover.jpg");
    assert_eq!(cover.imageable_type, "post");
}

#[tokio::test]
async fn morph_one_returns_none_when_absent() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MorphPost::create(attrs! { title: "p" }).await.unwrap();
    let cover = p.cover().first().await.unwrap();
    assert!(cover.is_none());
}

#[tokio::test]
async fn morph_many_eager_load() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p1 = MorphPost::create(attrs! { title: "p1" }).await.unwrap();
    let p2 = MorphPost::create(attrs! { title: "p2" }).await.unwrap();
    let _ = MorphComment::create(attrs! {
        commentable_id: p1.id, commentable_type: "post", body: "a"
    })
    .await
    .unwrap();
    let _ = MorphComment::create(attrs! {
        commentable_id: p1.id, commentable_type: "post", body: "b"
    })
    .await
    .unwrap();
    let _ = MorphComment::create(attrs! {
        commentable_id: p2.id, commentable_type: "post", body: "c"
    })
    .await
    .unwrap();
    // Noise: a comment attached to a video with the SAME PK as p1.
    // The eager-load query must filter by morph_type so this comment
    // doesn't get distributed into p1's group.
    let _ = MorphComment::create(attrs! {
        commentable_id: p1.id, commentable_type: "video", body: "noise"
    })
    .await
    .unwrap();

    let posts = MorphPost::with(["comments"]).get().await.unwrap();
    assert_eq!(posts.len(), 2);
    let p1_loaded = posts.iter().find(|p| p.id == p1.id).unwrap();
    assert_eq!(p1_loaded.comments_loaded().len(), 2);
    assert!(p1_loaded
        .comments_loaded()
        .iter()
        .all(|c| c.commentable_type == "post"));
    let p2_loaded = posts.iter().find(|p| p.id == p2.id).unwrap();
    assert_eq!(p2_loaded.comments_loaded().len(), 1);
}

#[tokio::test]
async fn morph_many_eager_load_empty_parent_gets_empty_slice() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p_with = MorphPost::create(attrs! { title: "with" }).await.unwrap();
    let p_without = MorphPost::create(attrs! { title: "without" })
        .await
        .unwrap();
    let _ = MorphComment::create(attrs! {
        commentable_id: p_with.id, commentable_type: "post", body: "only"
    })
    .await
    .unwrap();

    let posts = MorphPost::with(["comments"]).get().await.unwrap();
    let without_loaded = posts.iter().find(|p| p.id == p_without.id).unwrap();
    assert!(without_loaded.comments_loaded().is_empty());
}

#[tokio::test]
async fn morph_many_count_uses_server_side_group_by() {
    // The dispatcher arm issues a single GROUP BY query against the
    // child table — no client-side row buffering. We can't observe
    // the SQL directly here, but we can confirm the per-parent count
    // is right (including parents with zero children).
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p1 = MorphPost::create(attrs! { title: "p1" }).await.unwrap();
    let p2 = MorphPost::create(attrs! { title: "p2" }).await.unwrap();
    let p3 = MorphPost::create(attrs! { title: "p3" }).await.unwrap();
    for i in 0..3 {
        let _ = MorphComment::create(attrs! {
            commentable_id: p1.id,
            commentable_type: "post",
            body: format!("c{i}"),
        })
        .await
        .unwrap();
    }
    let _ = MorphComment::create(attrs! {
        commentable_id: p2.id, commentable_type: "post", body: "only"
    })
    .await
    .unwrap();
    // p3 has zero comments.

    let mut posts = MorphPost::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut MorphPost> = posts.iter_mut().collect();
        MorphPost::__count_relation("comments", refs.as_mut_slice(), _db.conn())
            .await
            .unwrap();
    }
    for p in posts.iter() {
        let expected = match p.id {
            id if id == p1.id => 3,
            id if id == p2.id => 1,
            id if id == p3.id => 0,
            _ => unreachable!(),
        };
        assert_eq!(p.comments_count(), expected, "post {} count", p.id);
    }
}

#[tokio::test]
async fn morph_many_count_filters_by_morph_type() {
    // The count dispatcher applies the morph-type predicate too.
    // p1 and v1 share the same PK (1) — Post.comments_count must NOT
    // include the Video's comment.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p1 = MorphPost::create(attrs! { title: "post1" }).await.unwrap();
    let v1 = MorphVideo::create(attrs! { url: "vid1.mp4" }).await.unwrap();
    assert_eq!(p1.id, v1.id, "test relies on collision of PKs across families");
    let _ = MorphComment::create(attrs! {
        commentable_id: p1.id,
        commentable_type: "post",
        body: "for post",
    })
    .await
    .unwrap();
    let _ = MorphComment::create(attrs! {
        commentable_id: v1.id,
        commentable_type: "video",
        body: "for video",
    })
    .await
    .unwrap();

    let mut posts = MorphPost::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut MorphPost> = posts.iter_mut().collect();
        MorphPost::__count_relation("comments", refs.as_mut_slice(), _db.conn())
            .await
            .unwrap();
    }
    let p_loaded = posts.iter().find(|p| p.id == p1.id).unwrap();
    assert_eq!(
        p_loaded.comments_count(),
        1,
        "count must exclude the video's comment even though PK matches"
    );
}

#[tokio::test]
async fn morph_many_aggregate_via_server_side_group_by() {
    // SUM(views) over Post.comments — also confirms the type-filter
    // applies to aggregates (video's comments with views=999 must not
    // be in the sum).
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p1 = MorphPost::create(attrs! { title: "p1" }).await.unwrap();
    let v1 = MorphVideo::create(attrs! { url: "vid1.mp4" }).await.unwrap();
    assert_eq!(p1.id, v1.id);
    // Two post comments with views = 5 + 10. Sum = 15.
    _db.execute_unprepared(&format!(
        "INSERT INTO morph_comments (commentable_id, commentable_type, body, views) VALUES \
         ({pid}, 'post', 'c1', 5), \
         ({pid}, 'post', 'c2', 10), \
         ({vid}, 'video', 'noise', 999)",
        pid = p1.id,
        vid = v1.id,
    ))
    .await
    .unwrap();

    let mut posts = MorphPost::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut MorphPost> = posts.iter_mut().collect();
        MorphPost::__aggregate_relation(
            "comments",
            "views",
            AggregateKind::Sum,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let p_loaded = posts.iter().find(|p| p.id == p1.id).unwrap();
    // The aggregate result lands in `__eager` keyed by the wide
    // `<rel>_<kind>_<col>` form (P1 fix). The `<rel>_count()` accessor
    // reads from `set_count`, so we read the raw aggregate cell
    // directly. SUM stored as f64.
    let sum: f64 = p_loaded
        .__eager
        .get_aggregate::<f64>("comments_sum_views")
        .copied()
        .unwrap_or(0.0);
    assert_eq!(sum, 15.0, "video's comment must be excluded from the sum");
}
