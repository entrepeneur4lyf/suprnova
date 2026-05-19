//! Phase 10A T9 — Auto-managed timestamps + `touch()`.
//!
//! When a model has both `created_at` and `updated_at` fields the
//! macro auto-detects timestamps and:
//! - sets BOTH to `Utc::now()` on `create()`
//! - bumps `updated_at` on every `save()` (and `update(attrs)`)
//! - emits a `Touchable` impl so `user.touch().await?` updates
//!   `updated_at` without changing any other column.
//!
//! Auto-detect: if the struct has NEITHER column the macro skips
//! injection silently (Laravel-parity for pivots / join tables /
//! no-history models). If the struct has EXACTLY ONE of the two
//! the macro emits a `compile_error!` — almost certainly a typo.
//!
//! `#[model(timestamps = false)]` is the explicit opt-out and works
//! regardless of which columns are on the struct.
//!
//! `#[model(touches = ["post"])]` parses and stores; the runtime
//! cascade is wired in Phase 10B once relations land.

use chrono::{DateTime, Utc};
use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, Model, Touchable};

// ---- Models ------------------------------------------------------------

// Default `timestamps = true` (implicit via auto-detect since the
// struct carries both columns).
#[model(table = "t9_users", fillable = ["name"])]
pub struct T9User {
    pub id: i64,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Explicit opt-out via `timestamps = false`. Struct has neither
// column; this exercises the opt-out branch independently of the
// auto-detect skip case below.
#[model(table = "t9_no_ts", fillable = ["name"], timestamps = false)]
pub struct T9NoTs {
    pub id: i64,
    pub name: String,
}

// Auto-detect skip: default `timestamps = true` but the struct lacks
// both columns, so the macro silently disables injection.
#[model(table = "t9_auto_skip", fillable = ["label"])]
pub struct T9AutoSkip {
    pub id: i64,
    pub label: String,
}

// `touches = ["post"]` parses without error in 10A. Runtime cascade
// activates in 10B once relations land.
#[model(table = "t9_comments", fillable = ["post_id", "body"], touches = ["post"])]
pub struct T9Comment {
    pub id: i64,
    pub post_id: i64,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---- Helpers -----------------------------------------------------------

async fn migrate_users(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE t9_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
}

// ---- Tests -------------------------------------------------------------

#[tokio::test]
async fn create_sets_both_timestamps() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_users(&db).await;

    let u = T9User::create(attrs! { name: "Alice" }).await.unwrap();
    let now = Utc::now();
    assert!(
        (now - u.created_at).num_seconds().abs() < 5,
        "created_at not within 5s of now: created_at={} now={}",
        u.created_at,
        now,
    );
    assert!(
        (now - u.updated_at).num_seconds().abs() < 5,
        "updated_at not within 5s of now: updated_at={} now={}",
        u.updated_at,
        now,
    );
}

#[tokio::test]
async fn save_bumps_updated_at_only() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_users(&db).await;

    let original = T9User::create(attrs! { name: "Alice" }).await.unwrap();
    let original_created = original.created_at;
    let original_updated = original.updated_at;

    // Sleep enough that a re-read's `updated_at` is observably newer
    // than the original. 1.2s comfortably exceeds the 1-second
    // resolution of SeaORM's chrono->TEXT format.
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    let mut handle = original.clone();
    handle.name = "Alice B".into();
    handle.save().await.unwrap();

    let reread = T9User::find(handle.id).await.unwrap().unwrap();
    assert_eq!(
        reread.created_at, original_created,
        "save() must NOT touch created_at"
    );
    assert!(
        reread.updated_at > original_updated,
        "save() must bump updated_at: reread.updated_at={} original={}",
        reread.updated_at,
        original_updated,
    );
    assert_eq!(reread.name, "Alice B");
}

#[tokio::test]
async fn update_attrs_bumps_updated_at_only() {
    // Covers the `Model::update(attrs)` path which routes through
    // `apply_attrs_to_active_model` rather than
    // `into_active_model_for_update`. The injection must catch BOTH
    // create() and update(attrs) — they share the apply_attrs hook.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_users(&db).await;

    let original = T9User::create(attrs! { name: "Alice" }).await.unwrap();
    let original_created = original.created_at;
    let original_updated = original.updated_at;
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    let updated = original.update(attrs! { name: "Alice B" }).await.unwrap();

    assert_eq!(
        updated.created_at, original_created,
        "update(attrs) must NOT touch created_at"
    );
    assert!(
        updated.updated_at > original_updated,
        "update(attrs) must bump updated_at: updated.updated_at={} original={}",
        updated.updated_at,
        original_updated,
    );
    assert_eq!(updated.name, "Alice B");
}

#[tokio::test]
async fn touch_bumps_updated_at_without_other_changes() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_users(&db).await;

    let u = T9User::create(attrs! { name: "Alice" }).await.unwrap();
    let original_updated = u.updated_at;
    let original_created = u.created_at;
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    u.touch().await.unwrap();

    let reread = T9User::find(u.id).await.unwrap().unwrap();
    assert!(
        reread.updated_at > original_updated,
        "touch() must bump updated_at: reread.updated_at={} original={}",
        reread.updated_at,
        original_updated,
    );
    assert_eq!(
        reread.created_at, original_created,
        "touch() must NOT touch created_at"
    );
    assert_eq!(reread.name, "Alice", "touch() must NOT change other columns");
}

#[tokio::test]
async fn timestamps_disabled_via_attribute() {
    // Struct has neither column AND `timestamps = false`. The opt-out
    // is explicit; auto-detect would also skip here, but this branch
    // remains the canonical way to disable timestamps for models that
    // DO carry created_at/updated_at columns for unrelated reasons.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t9_no_ts (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let u = T9NoTs::create(attrs! { name: "Alice" }).await.unwrap();
    assert_eq!(u.name, "Alice");
}

#[tokio::test]
async fn timestamps_auto_detect_skips_when_fields_absent() {
    // Default `timestamps = true` + struct lacks both columns →
    // macro auto-detects and skips injection silently. No
    // `timestamps = false` opt-out needed.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t9_auto_skip (id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let u = T9AutoSkip::create(attrs! { label: "x" }).await.unwrap();
    assert_eq!(u.label, "x");
}

#[tokio::test]
async fn touches_attribute_parses_for_phase_10b() {
    // `touches = ["post"]` parses without error. The runtime cascade
    // is a no-op in 10A; we test parse-compatibility + that a model
    // with `touches` still creates rows normally.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t9_comments (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            post_id INTEGER NOT NULL, \
            body TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();

    let c = T9Comment::create(attrs! { post_id: 1_i64, body: "Hello" })
        .await
        .unwrap();
    assert!(c.id > 0);
    // 10B will wire parent-touching post-save hooks; the macro
    // emission shape is asserted via the existence of the
    // `TOUCHES` const.
    assert_eq!(T9Comment::TOUCHES, &["post"]);
}
