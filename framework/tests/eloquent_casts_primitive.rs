//! Phase 10A T7a — Primitive + temporal casts.
//!
//! Each test declares a model at module scope (the `#[model]` macro
//! emits an inner module whose `use super::*;` only sees the test
//! file's top-level imports — putting models inside test functions
//! breaks cast-type resolution). The cast round-trips are asserted
//! via `Model::create` + `Model::find`; the `as_bool_round_trips`
//! test additionally asserts the storage shape with `db.fetch_one`.
//!
//! T7b ships the structured + enum casts + `with_casts` runtime
//! override; T7c ships the encrypted + hashed casts.

use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use std::str::FromStr;
use suprnova::testing::TestDatabase;
use suprnova::{
    attrs, model, AsBool, AsDate, AsDateTime, AsDecimal, AsFloat, AsImmutableDate,
    AsImmutableDateTime, AsInt, AsString, AsTimestamp, Model,
};

// ---- Models -------------------------------------------------------------

#[model(
    table = "t7a_bool_only",
    timestamps = false,
    fillable = ["flag"],
    casts = { flag = AsBool }
)]
pub struct OnlyBool {
    pub id: i64,
    pub flag: bool,
}

#[model(
    table = "t7a_int",
    timestamps = false,
    fillable = ["n"],
    casts = { n = AsInt<i32> }
)]
pub struct OnlyInt {
    pub id: i64,
    pub n: i32,
}

#[model(
    table = "t7a_float",
    timestamps = false,
    fillable = ["x"],
    casts = { x = AsFloat }
)]
pub struct OnlyFloat {
    pub id: i64,
    pub x: f64,
}

#[model(
    table = "t7a_decimal",
    timestamps = false,
    fillable = ["amount"],
    casts = { amount = AsDecimal<2> }
)]
pub struct OnlyDecimal {
    pub id: i64,
    pub amount: Decimal,
}

#[model(
    table = "t7a_string",
    timestamps = false,
    fillable = ["s"],
    casts = { s = AsString }
)]
pub struct OnlyString {
    pub id: i64,
    pub s: String,
}

#[model(
    table = "t7a_date",
    timestamps = false,
    fillable = ["d"],
    casts = { d = AsDate }
)]
pub struct OnlyDate {
    pub id: i64,
    pub d: NaiveDate,
}

#[model(
    table = "t7a_dt",
    timestamps = false,
    fillable = ["t"],
    casts = { t = AsDateTime }
)]
pub struct OnlyDt {
    pub id: i64,
    pub t: chrono::DateTime<Utc>,
}

#[model(
    table = "t7a_imm_d",
    timestamps = false,
    fillable = ["d"],
    casts = { d = AsImmutableDate }
)]
pub struct OnlyImmDate {
    pub id: i64,
    pub d: NaiveDate,
}

#[model(
    table = "t7a_imm_dt",
    timestamps = false,
    fillable = ["t"],
    casts = { t = AsImmutableDateTime }
)]
pub struct OnlyImmDt {
    pub id: i64,
    pub t: chrono::DateTime<Utc>,
}

#[model(
    table = "t7a_ts",
    timestamps = false,
    fillable = ["epoch"],
    casts = { epoch = AsTimestamp }
)]
pub struct OnlyTs {
    pub id: i64,
    pub epoch: i64,
}

// ---- Tests --------------------------------------------------------------

#[tokio::test]
async fn as_bool_round_trips() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7a_bool_only (id INTEGER PRIMARY KEY AUTOINCREMENT, flag INTEGER NOT NULL)",
    )
    .await
    .unwrap();
    let made = OnlyBool::create(attrs! { flag: true }).await.unwrap();
    let read = OnlyBool::find(made.id).await.unwrap().unwrap();
    assert!(read.flag);

    // Verify storage shape — boolean came back as integer 1.
    let raw = db
        .fetch_one(
            "SELECT flag FROM t7a_bool_only WHERE id = ?",
            vec![sea_orm::Value::from(made.id)],
        )
        .await
        .unwrap();
    let stored: i64 = raw.try_get("", "flag").unwrap();
    assert_eq!(stored, 1);
}

#[tokio::test]
async fn as_int_handles_i32_narrowing() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7a_int (id INTEGER PRIMARY KEY AUTOINCREMENT, n INTEGER NOT NULL)",
    )
    .await
    .unwrap();
    let made = OnlyInt::create(attrs! { n: 12345_i32 }).await.unwrap();
    let read = OnlyInt::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.n, 12345_i32);
}

#[tokio::test]
async fn as_float_round_trips() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7a_float (id INTEGER PRIMARY KEY AUTOINCREMENT, x REAL NOT NULL)",
    )
    .await
    .unwrap();
    // 2.5 picked deliberately to dodge clippy's `approx_constant` (3.14 ≈ π)
    // — the cast is a no-op pass-through; the value just needs to be a
    // representable f64 that survives binary round-trip.
    let made = OnlyFloat::create(attrs! { x: 2.5_f64 }).await.unwrap();
    let read = OnlyFloat::find(made.id).await.unwrap().unwrap();
    assert!((read.x - 2.5).abs() < 1e-9);
}

#[tokio::test]
async fn as_decimal_rounds_to_n_places() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7a_decimal (id INTEGER PRIMARY KEY AUTOINCREMENT, amount TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let made = OnlyDecimal::create(attrs! { amount: "9.99999" }).await.unwrap();
    let read = OnlyDecimal::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.amount, Decimal::from_str("10.00").unwrap());
}

#[tokio::test]
async fn as_string_pass_through() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7a_string (id INTEGER PRIMARY KEY AUTOINCREMENT, s TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let made = OnlyString::create(attrs! { s: "hello" }).await.unwrap();
    let read = OnlyString::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.s, "hello");
}

#[tokio::test]
async fn as_date_round_trips() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7a_date (id INTEGER PRIMARY KEY AUTOINCREMENT, d TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let made = OnlyDate::create(attrs! { d: "1990-01-15" }).await.unwrap();
    let read = OnlyDate::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.d, NaiveDate::from_ymd_opt(1990, 1, 15).unwrap());
}

#[tokio::test]
async fn as_datetime_round_trips() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7a_dt (id INTEGER PRIMARY KEY AUTOINCREMENT, t TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let when = chrono::Utc::now();
    let made = OnlyDt::create(attrs! { t: when.to_rfc3339() }).await.unwrap();
    let read = OnlyDt::find(made.id).await.unwrap().unwrap();
    assert!((read.t - when).num_milliseconds().abs() < 1000);
}

#[tokio::test]
async fn as_immutable_date_round_trips() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7a_imm_d (id INTEGER PRIMARY KEY AUTOINCREMENT, d TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let made = OnlyImmDate::create(attrs! { d: "2026-05-19" }).await.unwrap();
    let read = OnlyImmDate::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.d, NaiveDate::from_ymd_opt(2026, 5, 19).unwrap());
}

#[tokio::test]
async fn as_immutable_datetime_round_trips() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7a_imm_dt (id INTEGER PRIMARY KEY AUTOINCREMENT, t TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let when = chrono::Utc::now();
    let made = OnlyImmDt::create(attrs! { t: when.to_rfc3339() }).await.unwrap();
    let read = OnlyImmDt::find(made.id).await.unwrap().unwrap();
    assert!((read.t - when).num_milliseconds().abs() < 1000);
}

#[tokio::test]
async fn as_timestamp_round_trips_epoch_seconds() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t7a_ts (id INTEGER PRIMARY KEY AUTOINCREMENT, epoch INTEGER NOT NULL)",
    )
    .await
    .unwrap();
    let made = OnlyTs::create(attrs! { epoch: 1715200000_i64 }).await.unwrap();
    let read = OnlyTs::find(made.id).await.unwrap().unwrap();
    assert_eq!(read.epoch, 1715200000);
}
