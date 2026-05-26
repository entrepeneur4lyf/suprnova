//! Phase 10A T7b — Structured + enum casts + `with_casts` runtime
//! override.
//!
//! Same model-hoisting convention as T7a: each test's model lives at
//! module scope so the `#[model]` macro's inner module (which only
//! sees the test file's top-level `use` items) resolves the cast type
//! names correctly. The 5 structured casts round-trip Vec / HashMap /
//! Collection / serde_json::Value / IndexMap shapes; `AsEnum` rides on
//! `strum::EnumString` + `strum::AsRefStr` for FromStr / AsRef<str>
//! cleanly without a custom impl. The final two tests exercise the
//! `Builder<M>::with_casts(...)` runtime override path: one with
//! well-formed data (proves the pipeline runs end-to-end), one with
//! malformed data (proves the cast actually fires — without it the
//! query succeeds; with it the cast errors).
//!
//! T7c finishes the cast surface with encrypted + hashed casts.

use chrono::Utc;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use suprnova::testing::TestDatabase;
use suprnova::{
    AsArray, AsArrayObject, AsBool, AsCollection, AsDate, AsDateTime, AsEnum, AsInt, AsJson,
    AsObject, Collection, Model, attrs, model,
};

// ---- Test fixtures hoisted to module scope ------------------------------

#[derive(
    Serialize, Deserialize, Clone, Debug, PartialEq, Default, strum::EnumString, strum::AsRefStr,
)]
pub enum Role {
    #[default]
    Admin,
    Member,
    Guest,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Prefs {
    pub theme: String,
    pub notifications: bool,
}

// ---- Models -------------------------------------------------------------

#[model(
    table = "t7b_arr",
    timestamps = false,
    fillable = ["tags"],
    casts = { tags = AsArray<String> }
)]
pub struct ArrModel {
    pub id: i64,
    pub tags: Vec<String>,
}

#[model(
    table = "t7b_obj",
    timestamps = false,
    fillable = ["prefs"],
    casts = { prefs = AsObject<Prefs> }
)]
pub struct ObjModel {
    pub id: i64,
    pub prefs: Prefs,
}

#[model(
    table = "t7b_col",
    timestamps = false,
    fillable = ["items"],
    casts = { items = AsCollection<String> }
)]
pub struct ColModel {
    pub id: i64,
    pub items: Collection<String>,
}

#[model(
    table = "t7b_json",
    timestamps = false,
    fillable = ["payload"],
    casts = { payload = AsJson<serde_json::Value> }
)]
pub struct JsonModel {
    pub id: i64,
    pub payload: serde_json::Value,
}

#[model(
    table = "t7b_ao",
    timestamps = false,
    fillable = ["labels"],
    casts = { labels = AsArrayObject<String> }
)]
pub struct AoModel {
    pub id: i64,
    pub labels: IndexMap<String, String>,
}

#[model(
    table = "t7b_enum",
    timestamps = false,
    fillable = ["role"],
    casts = { role = AsEnum<Role> }
)]
pub struct EnumModel {
    pub id: i64,
    pub role: Role,
}

#[model(table = "t7b_wc1", timestamps = false, fillable = ["ts"])]
pub struct WcSanityModel {
    pub id: i64,
    pub ts: String,
}

#[model(table = "t7b_wc2", timestamps = false, fillable = ["s"])]
pub struct WcParseFailModel {
    pub id: i64,
    pub s: String,
}

// Override-semantic fixture. Static `AsBool` (i64 ↔ bool) on `flag`,
// so the storage shape (`i64`) differs from the runtime shape (`bool`).
// The override-semantic test below replaces the static cast with a
// runtime `AsInt<i64>` cast and asserts the runtime cast saw the
// storage shape — proving runtime casts bypass static casts entirely,
// not stack on top of them.
#[model(
    table = "t7b_override",
    timestamps = false,
    fillable = ["flag"],
    casts = { flag = AsBool }
)]
pub struct OverrideModel {
    pub id: i64,
    pub flag: bool,
}

// ---- Tests --------------------------------------------------------------

#[tokio::test]
async fn as_array_round_trips_vec() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7b_arr (id INTEGER PRIMARY KEY AUTOINCREMENT, tags TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let made = ArrModel::create(attrs! { tags: ["rust", "web"] })
        .await
        .unwrap();
    let read = ArrModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.tags, vec!["rust".to_string(), "web".to_string()]);
}

#[tokio::test]
async fn as_object_round_trips_custom_struct() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7b_obj (id INTEGER PRIMARY KEY AUTOINCREMENT, prefs TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let made = ObjModel::create(
        attrs! { prefs: serde_json::json!({ "theme": "dark", "notifications": true }) },
    )
    .await
    .unwrap();
    let read = ObjModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.prefs.theme, "dark");
    assert!(read.prefs.notifications);
}

#[tokio::test]
async fn as_collection_wraps_vec_in_collection() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7b_col (id INTEGER PRIMARY KEY AUTOINCREMENT, items TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let made = ColModel::create(attrs! { items: ["a", "b", "c"] })
        .await
        .unwrap();
    let read = ColModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.items.len(), 3);
    // Deref to slice works — Collection<T>::Deref::Target = [T].
    assert_eq!(&read.items[0], "a");
}

#[tokio::test]
async fn as_json_preserves_raw_value() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7b_json (id INTEGER PRIMARY KEY AUTOINCREMENT, payload TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let made = JsonModel::create(
        attrs! { payload: serde_json::json!({ "count": 42, "nested": { "ok": true } }) },
    )
    .await
    .unwrap();
    let read = JsonModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.payload["count"], 42);
    assert_eq!(read.payload["nested"]["ok"], true);
}

#[tokio::test]
async fn as_array_object_preserves_string_keys() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7b_ao (id INTEGER PRIMARY KEY AUTOINCREMENT, labels TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let made =
        AoModel::create(attrs! { labels: serde_json::json!({ "color": "blue", "size": "large" }) })
            .await
            .unwrap();
    let read = AoModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.labels.get("color"), Some(&"blue".to_string()));
    assert_eq!(read.labels.get("size"), Some(&"large".to_string()));
}

#[tokio::test]
async fn as_enum_round_trips() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7b_enum (id INTEGER PRIMARY KEY AUTOINCREMENT, role TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let made = EnumModel::create(attrs! { role: "Admin" }).await.unwrap();
    let read = EnumModel::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.role, Role::Admin);
}

#[tokio::test]
async fn with_casts_sanity_succeeds_against_well_formed_data() {
    // Smoke check: with a well-formed timestamp, the runtime cast
    // pipeline fires and the query succeeds. The unambiguous proof
    // that the pipeline actually ran is in the next test (which uses
    // a malformed value to force the cast to error).
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7b_wc1 (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let when = Utc::now();
    WcSanityModel::create(attrs! { ts: when.to_rfc3339() })
        .await
        .unwrap();

    let rows = WcSanityModel::query()
        .with_casts(suprnova::casts! { ts = AsDateTime })
        .get()
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn with_casts_bypasses_static_casts_entirely() {
    // Override semantics: when `with_casts(...)` is set, the static
    // cast pipeline is bypassed *entirely*, not stacked on top. To
    // prove this we use a model with `static AsBool` on a `bool` field,
    // then call `with_casts` setting an unrelated column. If the static
    // pipeline were still running, the bool field would land in `M` as
    // `true`/`false`. With the bypass semantic it lands in raw storage
    // shape (`i64`), which fails to deserialize into `M.flag: bool` —
    // proving the static cast did NOT run.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7b_override (id INTEGER PRIMARY KEY AUTOINCREMENT, flag INTEGER NOT NULL)",
    )
    .await
    .unwrap();
    let made = OverrideModel::create(attrs! { flag: true }).await.unwrap();

    // Baseline: without runtime casts, static AsBool fires and the
    // value round-trips correctly.
    let plain = OverrideModel::find(made.id).await.unwrap().unwrap();
    assert!(
        plain.flag,
        "static cast should round-trip without with_casts"
    );

    // With a runtime cast set on an unrelated column (`id` here), the
    // static cast pipeline is bypassed for ALL columns. The `flag`
    // column comes back as raw i64 (storage shape), which fails to
    // deserialize into the user's `bool` field — surfacing as a
    // FrameworkError.
    let result = OverrideModel::query()
        .with_casts(suprnova::casts! { id = AsInt<i64> })
        .get()
        .await;
    assert!(
        result.is_err(),
        "expected runtime override to bypass static AsBool and fail bool/i64 deserialization"
    );
}

#[tokio::test]
async fn with_casts_pipeline_actually_runs_proven_by_parse_failure() {
    // Stored data: a non-date string. Without a runtime cast, the
    // query succeeds (the model's String field accepts anything). With
    // an AsDate runtime cast, the pipeline tries to parse "not-a-date"
    // as a NaiveDate at decode time and the parse fails — surfacing as
    // a FrameworkError. This unambiguously proves the cast actually ran.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7b_wc2 (id INTEGER PRIMARY KEY AUTOINCREMENT, s TEXT NOT NULL)",
    )
    .await
    .unwrap();
    WcParseFailModel::create(attrs! { s: "not-a-date" })
        .await
        .unwrap();

    // Baseline: without the cast, the query succeeds.
    let plain = WcParseFailModel::query().get().await.unwrap();
    assert_eq!(plain.len(), 1);
    assert_eq!(plain[0].s, "not-a-date");

    // With the runtime cast: pipeline fires, parse fails, query errors.
    let result = WcParseFailModel::query()
        .with_casts(suprnova::casts! { s = AsDate })
        .get()
        .await;
    assert!(
        result.is_err(),
        "expected runtime AsDate cast to error on \"not-a-date\""
    );
    let msg = format!("{}", result.unwrap_err()).to_lowercase();
    assert!(
        msg.contains("date") || msg.contains("parse"),
        "expected date/parse mention in error, got: {msg}"
    );
}
