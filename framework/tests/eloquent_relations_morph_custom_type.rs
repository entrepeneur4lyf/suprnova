//! Phase 10B P2 — MorphTo dispatch consults the T8 `MorphTypeEntry`
//! registry, not the structural heuristic keys.
//!
//! Before P2, the per-family fetch helper matched `self.morph_type`
//! against keys derived from each target's type name
//! (snake / no-underscore / Laravel-prefix-stripped). A user who
//! declared `#[suprnova::model(morph_type = "blog_post")]` on a
//! target named `MorphBlogPost` got `"morph_blog_post"` /
//! `"morphblogpost"` / `"blog_post"` as the candidate keys — the
//! third happened to match by coincidence of the prefix-stripping
//! rule. Pick almost any other custom `morph_type` string (e.g.
//! `"article_v2"` on a struct named `LegacyPost`) and dispatch
//! silently fell through to `Unknown`.
//!
//! P2 changes the emission to consult
//! `find_morph_type_by_id::<Target>()` for each declared target and
//! compare the registered `morph_type` string against the runtime
//! `self.morph_type`. First match wins.
//!
//! This test pins the custom-`morph_type` path end-to-end: the
//! target struct carries an arbitrary `morph_type` value the
//! structural heuristics would NEVER have produced, and dispatch
//! still lands in the correct variant.
//!
//! Coverage matrix:
//!
//! - `morph_to_dispatches_via_custom_morph_type_string` — happy
//!   path, custom morph_type → correct variant via registry.
//! - `morph_to_dispatches_to_second_target_via_registry` — same,
//!   second target, ensuring dispatch doesn't bias toward the
//!   first arm.
//! - `morph_to_dispatches_via_snake_fallback_for_implicit_default_target`
//!   — target without an explicit `morph_type` attribute is absent
//!   from the registry; emission falls back to
//!   `to_snake(target_type_name)`, matching the parent-side write
//!   convention. Pins the implicit-default contract end-to-end.
//! - `morph_to_unknown_when_morph_type_string_unknown` —
//!   negative-space check: a stored type string that no target
//!   registers falls through to `Unknown`. Confirms the dispatcher
//!   doesn't accidentally match by structural similarity.

use suprnova::testing::TestDatabase;
use suprnova::{Model, attrs, model};

// The "blog_post" morph_type string is NOT one the old heuristic
// emission would have produced for the type name `MorphBlogPost`.
// `morph_target_keys(MorphBlogPost)` yielded `["morph_blog_post",
// "morphblogpost", "blog_post"]` — the third matches by coincidence
// of the `Morph` prefix-stripping rule. To make the test bite, we
// pick a target struct name whose stripping rule WOULDN'T produce
// the registered morph_type — `LegacyArticle` with
// `morph_type = "blog_post"` so heuristic keys are
// `["legacy_article", "legacyarticle"]`, neither matching the
// runtime string.
#[model(table = "p2_articles", morph_type = "blog_post")]
pub struct LegacyArticle {
    pub id: i64,
    pub title: String,
}

// A second target exercising the same custom-string path, to pin
// that the registry-driven dispatch correctly distinguishes BETWEEN
// targets (not just "any non-Unknown variant"). Target name +
// morph_type are deliberately mismatched.
#[model(table = "p2_videos", morph_type = "media_clip")]
pub struct AncientVideo {
    pub id: i64,
    pub url: String,
}

// The MorphTo declaration. `targets` are listed by their type
// names; the macro can't see the targets' `morph_type` attributes
// at expansion time, so dispatch HAS to defer to the runtime
// registry.
#[model(table = "p2_comments", relations = {
    parent: MorphTo { name = "parent", targets = [LegacyArticle, AncientVideo] },
})]
pub struct P2Comment {
    pub id: i64,
    pub parent_id: i64,
    pub parent_type: String,
    pub body: String,
}

// A second target WITHOUT an explicit `morph_type` attribute. This
// exercises the snake-cased type-name fallback the emission carries
// for implicit-default targets: `find_morph_type_by_id::<Plain>()`
// returns None (T8's registry doesn't track non-morph_type models),
// and dispatch compares `self.morph_type` against
// `to_snake("Plain") == "plain"` — the same default the parent-side
// `MorphMany` / `MorphOne` would use to STAMP the column.
#[model(table = "p2_plains")]
pub struct Plain {
    pub id: i64,
    pub name: String,
}

// Second child carrying the implicit-default target. Separate from
// `P2Comment` so the test isolation stays clean.
#[model(table = "p2_notes", relations = {
    target: MorphTo { name = "target", targets = [Plain] },
})]
pub struct P2Note {
    pub id: i64,
    pub target_id: i64,
    pub target_type: String,
    pub body: String,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE p2_articles (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE p2_videos (id INTEGER PRIMARY KEY AUTOINCREMENT, url TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE p2_comments (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            parent_id INTEGER NOT NULL, \
            parent_type TEXT NOT NULL, \
            body TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE p2_plains (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE p2_notes (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            target_id INTEGER NOT NULL, \
            target_type TEXT NOT NULL, \
            body TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn morph_to_dispatches_via_custom_morph_type_string() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let a = LegacyArticle::create(attrs! { title: "the future of rust" })
        .await
        .unwrap();
    // Insert a comment with parent_type = "blog_post" — the CUSTOM
    // morph_type LegacyArticle registers. Before P2, dispatch would
    // try heuristic keys `["legacy_article", "legacyarticle"]` and
    // miss; after P2, the registry resolves
    // `TypeId::of::<LegacyArticle>() → "blog_post"` and the
    // comparison hits.
    let c = P2Comment::create(attrs! {
        parent_id: a.id,
        parent_type: "blog_post",
        body: "hi",
    })
    .await
    .unwrap();

    match c.parent().get().await.unwrap() {
        ParentMorph::LegacyArticle(parent) => {
            assert_eq!(parent.id, a.id);
            assert_eq!(parent.title, "the future of rust");
        }
        other => {
            panic!("expected LegacyArticle variant for parent_type=\"blog_post\", got {other:?}")
        }
    }
}

#[tokio::test]
async fn morph_to_dispatches_to_second_target_via_registry() {
    // Same custom-string path, against the second declared target,
    // ensuring the dispatcher doesn't bias toward the first arm.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let v = AncientVideo::create(attrs! { url: "old.mp4" })
        .await
        .unwrap();
    let c = P2Comment::create(attrs! {
        parent_id: v.id,
        parent_type: "media_clip",
        body: "vid",
    })
    .await
    .unwrap();

    match c.parent().get().await.unwrap() {
        ParentMorph::AncientVideo(parent) => {
            assert_eq!(parent.id, v.id);
            assert_eq!(parent.url, "old.mp4");
        }
        other => {
            panic!("expected AncientVideo variant for parent_type=\"media_clip\", got {other:?}")
        }
    }
}

#[tokio::test]
async fn morph_to_dispatches_via_snake_fallback_for_implicit_default_target() {
    // Target struct `Plain` has NO `morph_type` attribute. T8's
    // registry deliberately excludes such structs (see
    // `morph_type_not_registered_for_non_morph_models`), so
    // `find_morph_type_by_id::<Plain>()` returns None. The
    // emission must fall back to comparing `self.morph_type`
    // against `to_snake("Plain") == "plain"` — the same default
    // the parent-side MorphMany / MorphOne uses to write the
    // type-string column. This pins the implicit-default contract
    // documented in `docs/core/eloquent.md#MorphTo` end-to-end:
    // removing the snake fallback in a future refactor would flip
    // this test red.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let p = Plain::create(attrs! { name: "p" }).await.unwrap();
    let n = P2Note::create(attrs! {
        target_id: p.id,
        target_type: "plain",
        body: "note",
    })
    .await
    .unwrap();

    match n.target().get().await.unwrap() {
        TargetMorph::Plain(parent) => {
            assert_eq!(parent.id, p.id);
            assert_eq!(parent.name, "p");
        }
        other => panic!(
            "expected Plain variant via snake-fallback for target_type=\"plain\", got {other:?}"
        ),
    }
}

#[tokio::test]
async fn morph_to_unknown_when_morph_type_string_unknown() {
    // Negative-space check: a `parent_type` that NO target
    // registers must surface `Unknown` — confirms the dispatcher
    // isn't matching by accidental structural similarity. The
    // stored "legacy_article" string happens to be the snake form
    // of LegacyArticle's type name (the structural fallback the
    // emission carries for implicit-default targets), but
    // LegacyArticle declares an explicit `morph_type = "blog_post"`
    // so the registry path takes precedence and the snake fallback
    // is unreachable for this target.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    _db.execute_unprepared(
        "INSERT INTO p2_comments (parent_id, parent_type, body) \
         VALUES (1, 'legacy_article', 'stale')",
    )
    .await
    .unwrap();
    let c = P2Comment::find(1).await.unwrap().expect("inserted comment");

    match c.parent().get().await.unwrap() {
        ParentMorph::Unknown(t, id) => {
            assert_eq!(t, "legacy_article");
            assert_eq!(id, 1);
        }
        other => panic!(
            "expected Unknown for parent_type=\"legacy_article\" (no target registers that string \
             — LegacyArticle's registered morph_type is \"blog_post\"), got {other:?}"
        ),
    }
}
