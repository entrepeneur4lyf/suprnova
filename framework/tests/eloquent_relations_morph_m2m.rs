//! Phase 10B T7 — `MorphToMany<L, R, P>` + `MorphedByMany<L, R, P>` +
//! polymorphic m2m.
//!
//! Exercises the full polymorphic m2m surface — the same shape as
//! `BelongsToMany` (T4) but with a `*_type` discriminator on every
//! pivot SQL statement so multiple parent morph families can share a
//! single pivot table:
//!
//! - `MorphToMany` (parent → m2m partner): `Post.tags()`,
//!   `Video.tags()`. Pivot rows match by parent id + type. Mutators:
//!   `.attach(id)` / `.attach_with(id, attrs!{...})` / `.detach(id)` /
//!   `.sync([...])`. Readers: `.get()` (two-query with `__pivot`),
//!   `.first()`, `.count()`.
//! - `MorphedByMany` (m2m partner → one specific morph target family):
//!   `Tag.posts()` returns only Post-typed taggables, `Tag.videos()`
//!   returns only Video-typed taggables. Readers only — pivot writes
//!   go through the parent-side `MorphToMany`.
//!
//! Eager loading runs through the parent model's `__eager_load`
//! dispatcher arm. Server-side GROUP BY for count + aggregate matches
//! the BelongsToMany contract: Sum/Avg → f64 / 0.0 empty, Min/Max →
//! Option<f64> / None empty.

use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, AggregateKind, Model};

#[model(table = "mm_tags", relations = {
    posts: MorphedByMany<MmPost, MmTaggable> {
        name = "taggable",
        target_morph_type = "post",
    },
    videos: MorphedByMany<MmVideo, MmTaggable> {
        name = "taggable",
        target_morph_type = "video",
    },
})]
pub struct MmTag {
    pub id: i64,
    pub name: String,
}

#[model(table = "mm_posts", morph_type = "post", relations = {
    tags: MorphToMany<MmTag, MmTaggable> { name = "taggable" },
})]
pub struct MmPost {
    pub id: i64,
    pub title: String,
}

#[model(table = "mm_videos", morph_type = "video", relations = {
    tags: MorphToMany<MmTag, MmTaggable> { name = "taggable" },
})]
pub struct MmVideo {
    pub id: i64,
    pub url: String,
}

#[model(table = "mm_taggables", primary_key = "id", timestamps = false)]
pub struct MmTaggable {
    pub id: i64,
    pub mm_tag_id: i64,
    pub taggable_id: i64,
    pub taggable_type: String,
    pub weight: Option<i64>,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE mm_tags (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE mm_posts (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            title TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE mm_videos (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            url TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE mm_taggables (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            mm_tag_id INTEGER NOT NULL, \
            taggable_id INTEGER NOT NULL, \
            taggable_type TEXT NOT NULL, \
            weight INTEGER, \
            UNIQUE(mm_tag_id, taggable_id, taggable_type))",
    )
    .await
    .unwrap();
}

// ---- attach / detach / sync (MorphToMany side) --------------------------

#[tokio::test]
async fn morph_to_many_attaches_to_post() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "rust" }).await.unwrap();
    p.tags().attach(t.id).await.unwrap();

    let tags = p.tags().get().await.unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].name, "rust");
}

#[tokio::test]
async fn morph_to_many_attaches_to_video_independently() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let v = MmVideo::create(attrs! { url: "u.mp4" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "shared" }).await.unwrap();
    p.tags().attach(t.id).await.unwrap();
    v.tags().attach(t.id).await.unwrap();

    let p_tags = p.tags().get().await.unwrap();
    let v_tags = v.tags().get().await.unwrap();
    assert_eq!(p_tags.len(), 1);
    assert_eq!(v_tags.len(), 1);
    // Same tag row, two morph attachments — proves the pivot's
    // `taggable_type` discriminator separates the two families.
    assert_eq!(p_tags[0].id, v_tags[0].id);
}

#[tokio::test]
async fn morph_to_many_detach() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "tag" }).await.unwrap();
    p.tags().attach(t.id).await.unwrap();
    p.tags().detach(t.id).await.unwrap();
    assert_eq!(p.tags().get().await.unwrap().len(), 0);
}

#[tokio::test]
async fn morph_to_many_detach_does_not_touch_other_family() {
    // Pin the detach contract: detaching a tag from a Post must NOT
    // remove the same tag's Video attachment. Without the type
    // discriminator on the DELETE, a naive `DELETE WHERE
    // mm_tag_id = ? AND taggable_id = ?` would wipe both rows if the
    // post and video happened to share an id.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let v = MmVideo::create(attrs! { url: "v" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "shared" }).await.unwrap();
    p.tags().attach(t.id).await.unwrap();
    v.tags().attach(t.id).await.unwrap();

    // Same primary-key value across two morph families is the worst
    // case — assert the detach only removed the matching type.
    assert_eq!(p.id, v.id, "test relies on Post.id == Video.id sharing values");

    p.tags().detach(t.id).await.unwrap();
    assert_eq!(p.tags().get().await.unwrap().len(), 0);
    assert_eq!(v.tags().get().await.unwrap().len(), 1);
}

#[tokio::test]
async fn morph_to_many_sync_transactional() {
    // `sync` is the diff-and-apply path: attach what's new, detach
    // what's gone. Mirrors BelongsToMany::sync — wraps in a single
    // transaction so partial failures roll back.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let t1 = MmTag::create(attrs! { name: "t1" }).await.unwrap();
    let t2 = MmTag::create(attrs! { name: "t2" }).await.unwrap();
    let t3 = MmTag::create(attrs! { name: "t3" }).await.unwrap();
    p.tags().attach(t1.id).await.unwrap();
    p.tags().attach(t2.id).await.unwrap();

    // sync([t2, t3]) → t1 detaches, t2 stays, t3 attaches.
    p.tags().sync([t2.id, t3.id]).await.unwrap();
    let after: Vec<i64> = p
        .tags()
        .get()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(after.contains(&t2.id));
    assert!(after.contains(&t3.id));
    assert!(!after.contains(&t1.id));
    assert_eq!(after.len(), 2);
}

#[tokio::test]
async fn morph_to_many_sync_does_not_leak_across_families() {
    // The SELECT inside `sync` must filter by `taggable_type`. If
    // it didn't, syncing post.tags() would see video attachments as
    // "current" and either skip a needed attach or trigger an
    // unwanted detach. Pin both halves of the contract.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let v = MmVideo::create(attrs! { url: "v" }).await.unwrap();
    let t1 = MmTag::create(attrs! { name: "t1" }).await.unwrap();
    let t2 = MmTag::create(attrs! { name: "t2" }).await.unwrap();
    // Pre-state: post has t1; video has both t1 and t2.
    p.tags().attach(t1.id).await.unwrap();
    v.tags().attach(t1.id).await.unwrap();
    v.tags().attach(t2.id).await.unwrap();

    // sync the post to [t2]. Must detach t1 from the post, attach t2.
    // Must NOT touch the video's attachments.
    p.tags().sync([t2.id]).await.unwrap();

    let p_tags: Vec<i64> = p.tags().get().await.unwrap().into_iter().map(|r| r.id).collect();
    assert_eq!(p_tags, vec![t2.id]);

    let mut v_tags: Vec<i64> = v.tags().get().await.unwrap().into_iter().map(|r| r.id).collect();
    v_tags.sort();
    let mut expected = vec![t1.id, t2.id];
    expected.sort();
    assert_eq!(v_tags, expected, "video attachments must not be touched by post.sync()");
}

// ---- MorphedByMany inverse direction -----------------------------------

#[tokio::test]
async fn morphed_by_many_inverse() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p1 = MmPost::create(attrs! { title: "p1" }).await.unwrap();
    let p2 = MmPost::create(attrs! { title: "p2" }).await.unwrap();
    let v = MmVideo::create(attrs! { url: "v.mp4" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "popular" }).await.unwrap();
    p1.tags().attach(t.id).await.unwrap();
    p2.tags().attach(t.id).await.unwrap();
    v.tags().attach(t.id).await.unwrap();

    // From the tag, fetch all posts (NOT videos).
    let posts = t.posts().get().await.unwrap();
    assert_eq!(posts.len(), 2);
    let post_ids: Vec<i64> = posts.iter().map(|p| p.id).collect();
    assert!(post_ids.contains(&p1.id));
    assert!(post_ids.contains(&p2.id));

    // From the tag, fetch all videos (NOT posts).
    let videos = t.videos().get().await.unwrap();
    assert_eq!(videos.len(), 1);
    assert_eq!(videos[0].id, v.id);
}

#[tokio::test]
async fn morphed_by_many_first_returns_one() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "t" }).await.unwrap();
    p.tags().attach(t.id).await.unwrap();

    let first = t.posts().first().await.unwrap();
    assert!(first.is_some());
    assert_eq!(first.unwrap().id, p.id);
}

#[tokio::test]
async fn morphed_by_many_count() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p1 = MmPost::create(attrs! { title: "p1" }).await.unwrap();
    let p2 = MmPost::create(attrs! { title: "p2" }).await.unwrap();
    let v = MmVideo::create(attrs! { url: "v.mp4" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "popular" }).await.unwrap();
    p1.tags().attach(t.id).await.unwrap();
    p2.tags().attach(t.id).await.unwrap();
    v.tags().attach(t.id).await.unwrap();

    // Lazy count filters by target morph type.
    assert_eq!(t.posts().count().await.unwrap(), 2);
    assert_eq!(t.videos().count().await.unwrap(), 1);
}

// ---- Eager loading -----------------------------------------------------

#[tokio::test]
async fn morph_to_many_eager_load() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p1 = MmPost::create(attrs! { title: "p1" }).await.unwrap();
    let p2 = MmPost::create(attrs! { title: "p2" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "x" }).await.unwrap();
    p1.tags().attach(t.id).await.unwrap();
    p2.tags().attach(t.id).await.unwrap();

    let posts = MmPost::with(["tags"]).get().await.unwrap();
    assert_eq!(posts.len(), 2);
    for p in posts.iter() {
        assert_eq!(p.tags_loaded().len(), 1);
        assert_eq!(p.tags_loaded()[0].id, t.id);
    }
}

/// Helper — invoke `__count_relation` directly. T9's user-facing
/// `with_count(["..."])` builder method lives on top of this hook;
/// driving the hook lets us pin the count contract before T9 lands the
/// surface sugar.
async fn dispatch_count<M>(
    rows: &mut [M],
    relation: &str,
    db: &sea_orm::DatabaseConnection,
) where
    M: suprnova::eloquent::EagerLoadDispatch,
{
    let mut refs: Vec<&mut M> = rows.iter_mut().collect();
    M::count_relation(relation, refs.as_mut_slice(), db)
        .await
        .unwrap();
}

/// Helper — invoke `__aggregate_relation` directly. Same rationale as
/// `dispatch_count`.
async fn dispatch_aggregate<M>(
    rows: &mut [M],
    relation: &str,
    column: &str,
    kind: AggregateKind,
    db: &sea_orm::DatabaseConnection,
) where
    M: suprnova::eloquent::EagerLoadDispatch,
{
    let mut refs: Vec<&mut M> = rows.iter_mut().collect();
    M::aggregate_relation(relation, column, kind, refs.as_mut_slice(), db)
        .await
        .unwrap();
}

#[tokio::test]
async fn morph_to_many_eager_load_excludes_other_family() {
    // The eager arm MUST apply the `<name>_type = '<self_morph_type>'`
    // filter — otherwise Post.tags eager-load would surface
    // video-side attachments shared on the same pivot.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let v = MmVideo::create(attrs! { url: "v" }).await.unwrap();
    let t_post = MmTag::create(attrs! { name: "post-only" })
        .await
        .unwrap();
    let t_video = MmTag::create(attrs! { name: "video-only" })
        .await
        .unwrap();
    p.tags().attach(t_post.id).await.unwrap();
    v.tags().attach(t_video.id).await.unwrap();

    let posts = MmPost::with(["tags"]).get().await.unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].tags_loaded().len(), 1);
    assert_eq!(posts[0].tags_loaded()[0].id, t_post.id);
}

#[tokio::test]
async fn morphed_by_many_eager_load() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p1 = MmPost::create(attrs! { title: "p1" }).await.unwrap();
    let p2 = MmPost::create(attrs! { title: "p2" }).await.unwrap();
    let v = MmVideo::create(attrs! { url: "v.mp4" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "popular" }).await.unwrap();
    p1.tags().attach(t.id).await.unwrap();
    p2.tags().attach(t.id).await.unwrap();
    v.tags().attach(t.id).await.unwrap();

    let tags = MmTag::with(["posts", "videos"]).get().await.unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].posts_loaded().len(), 2);
    assert_eq!(tags[0].videos_loaded().len(), 1);
}

// ---- Pivot accessor (THE key test) -------------------------------------

#[tokio::test]
async fn morph_to_many_pivot_accessor() {
    // The macro-emitted `.pivot::<P>()` accessor must work on R rows
    // returned by `MorphToMany::get()`. The pivot Arc downcast must
    // hit the user's Pivot type cleanly.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "rust" }).await.unwrap();
    p.tags().attach(t.id).await.unwrap();

    let tags = p.tags().get().await.unwrap();
    assert_eq!(tags.len(), 1);
    let pivot: &MmTaggable = tags[0].pivot::<MmTaggable>();
    assert_eq!(pivot.mm_tag_id, t.id);
    assert_eq!(pivot.taggable_id, p.id);
    assert_eq!(pivot.taggable_type, "post");
}

#[tokio::test]
async fn morph_to_many_pivot_accessor_after_eager_load() {
    // Eager loading must also stamp __pivot per attachment, matching
    // BelongsToMany's clone-per-attachment contract.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "rust" }).await.unwrap();
    p.tags().attach(t.id).await.unwrap();

    let posts = MmPost::with(["tags"]).get().await.unwrap();
    assert_eq!(posts.len(), 1);
    let loaded_tags = posts[0].tags_loaded();
    assert_eq!(loaded_tags.len(), 1);
    let pivot: &MmTaggable = loaded_tags[0].pivot::<MmTaggable>();
    assert_eq!(pivot.taggable_id, p.id);
    assert_eq!(pivot.taggable_type, "post");
}

#[tokio::test]
async fn morphed_by_many_pivot_accessor() {
    // Inverse-direction pivot accessor — `tag.posts().get()` should
    // stamp `__pivot` on each Post just like the parent direction.
    // Mirrors `MorphToMany`'s pivot contract for API symmetry.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "rust" }).await.unwrap();
    p.tags().attach(t.id).await.unwrap();

    let posts = t.posts().get().await.unwrap();
    assert_eq!(posts.len(), 1);
    let pivot: &MmTaggable = posts[0].pivot::<MmTaggable>();
    assert_eq!(pivot.mm_tag_id, t.id);
    assert_eq!(pivot.taggable_id, p.id);
    assert_eq!(pivot.taggable_type, "post");
}

#[tokio::test]
async fn morphed_by_many_pivot_accessor_after_eager_load() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    let t = MmTag::create(attrs! { name: "rust" }).await.unwrap();
    p.tags().attach(t.id).await.unwrap();

    let tags = MmTag::with(["posts"]).get().await.unwrap();
    assert_eq!(tags.len(), 1);
    let loaded_posts = tags[0].posts_loaded();
    assert_eq!(loaded_posts.len(), 1);
    let pivot: &MmTaggable = loaded_posts[0].pivot::<MmTaggable>();
    assert_eq!(pivot.mm_tag_id, t.id);
    assert_eq!(pivot.taggable_id, p.id);
    assert_eq!(pivot.taggable_type, "post");
}

// ---- Server-side count + aggregate -------------------------------------

#[tokio::test]
async fn morph_to_many_count_via_server_side_group_by() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p1 = MmPost::create(attrs! { title: "p1" }).await.unwrap();
    let p2 = MmPost::create(attrs! { title: "p2" }).await.unwrap();
    let v = MmVideo::create(attrs! { url: "v" }).await.unwrap();
    let t1 = MmTag::create(attrs! { name: "t1" }).await.unwrap();
    let t2 = MmTag::create(attrs! { name: "t2" }).await.unwrap();
    p1.tags().attach(t1.id).await.unwrap();
    p1.tags().attach(t2.id).await.unwrap();
    p2.tags().attach(t1.id).await.unwrap();
    // Video attachment must NOT show up in post-side count.
    v.tags().attach(t1.id).await.unwrap();
    v.tags().attach(t2.id).await.unwrap();

    let mut posts = MmPost::query().get().await.unwrap();
    dispatch_count(&mut posts, "tags", _db.conn()).await;
    let by_id: std::collections::HashMap<i64, u64> = posts
        .iter()
        .map(|p| (p.id, p.tags_count()))
        .collect();
    assert_eq!(by_id.get(&p1.id), Some(&2));
    assert_eq!(by_id.get(&p2.id), Some(&1));
}

#[tokio::test]
async fn morph_to_many_aggregate_via_server_side_group_by() {
    // Aggregate over the related table's column (Laravel parity —
    // users typically aggregate over tag.id-related metrics, not the
    // pivot's own columns).
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p1 = MmPost::create(attrs! { title: "p1" }).await.unwrap();
    let p2 = MmPost::create(attrs! { title: "p2" }).await.unwrap();
    let t1 = MmTag::create(attrs! { name: "t1" }).await.unwrap();
    let t2 = MmTag::create(attrs! { name: "t2" }).await.unwrap();
    let t3 = MmTag::create(attrs! { name: "t3" }).await.unwrap();
    p1.tags().attach(t1.id).await.unwrap();
    p1.tags().attach(t2.id).await.unwrap();
    p2.tags().attach(t3.id).await.unwrap();

    // SUM(tag.id) per post. p1 = t1.id + t2.id; p2 = t3.id.
    let expected_p1 = (t1.id + t2.id) as f64;
    let expected_p2 = t3.id as f64;

    let mut posts = MmPost::query().get().await.unwrap();
    dispatch_aggregate(&mut posts, "tags", "id", AggregateKind::Sum, _db.conn()).await;
    let by_id: std::collections::HashMap<i64, f64> = posts
        .iter()
        .map(|p| {
            (
                p.id,
                p.__eager
                    .get_aggregate::<f64>("tags_sum_id")
                    .copied()
                    .unwrap_or(0.0),
            )
        })
        .collect();
    assert_eq!(by_id.get(&p1.id).copied().unwrap_or(0.0), expected_p1);
    assert_eq!(by_id.get(&p2.id).copied().unwrap_or(0.0), expected_p2);
}

#[tokio::test]
async fn morph_to_many_aggregate_min_max_returns_option() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = MmPost::create(attrs! { title: "p" }).await.unwrap();
    // Empty-set Post — no attachments. Min/Max → None.
    let mut posts_no_tags = MmPost::query().get().await.unwrap();
    dispatch_aggregate(
        &mut posts_no_tags,
        "tags",
        "id",
        AggregateKind::Min,
        _db.conn(),
    )
    .await;
    let empty_min: Option<f64> = posts_no_tags
        .iter()
        .find(|x| x.id == p.id)
        .and_then(|x| {
            x.__eager
                .get_aggregate::<Option<f64>>("tags_min_id")
                .copied()
        })
        .unwrap_or(None);
    assert!(empty_min.is_none(), "empty min must be None, got {empty_min:?}");

    // Now attach two tags and re-run Max.
    let t1 = MmTag::create(attrs! { name: "t1" }).await.unwrap();
    let t2 = MmTag::create(attrs! { name: "t2" }).await.unwrap();
    p.tags().attach(t1.id).await.unwrap();
    p.tags().attach(t2.id).await.unwrap();

    let mut posts_with_tags = MmPost::query().get().await.unwrap();
    dispatch_aggregate(
        &mut posts_with_tags,
        "tags",
        "id",
        AggregateKind::Max,
        _db.conn(),
    )
    .await;
    let max: Option<f64> = posts_with_tags
        .iter()
        .find(|x| x.id == p.id)
        .and_then(|x| {
            x.__eager
                .get_aggregate::<Option<f64>>("tags_max_id")
                .copied()
        })
        .unwrap_or(None);
    assert_eq!(max, Some(t2.id.max(t1.id) as f64));
}

#[tokio::test]
async fn morphed_by_many_count_via_server_side_group_by() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p1 = MmPost::create(attrs! { title: "p1" }).await.unwrap();
    let p2 = MmPost::create(attrs! { title: "p2" }).await.unwrap();
    let v = MmVideo::create(attrs! { url: "v" }).await.unwrap();
    let t1 = MmTag::create(attrs! { name: "t1" }).await.unwrap();
    let t2 = MmTag::create(attrs! { name: "t2" }).await.unwrap();
    p1.tags().attach(t1.id).await.unwrap();
    p2.tags().attach(t1.id).await.unwrap();
    p1.tags().attach(t2.id).await.unwrap();
    // Video attachment must NOT show up in `posts` count from the tag side.
    v.tags().attach(t1.id).await.unwrap();

    let mut tags = MmTag::query().get().await.unwrap();
    dispatch_count(&mut tags, "posts", _db.conn()).await;
    let by_id: std::collections::HashMap<i64, u64> = tags
        .iter()
        .map(|t| (t.id, t.posts_count()))
        .collect();
    assert_eq!(by_id.get(&t1.id), Some(&2));
    assert_eq!(by_id.get(&t2.id), Some(&1));
}
