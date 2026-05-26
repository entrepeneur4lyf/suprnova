//! Phase 10C T9 — Row locking SQL emission.
//!
//! `Builder::lock_for_update` and `Builder::shared_lock` set a flag
//! that the SQL renderer materialises as the per-backend lock clause
//! at the very end of the compound statement (after every UNION arm
//! and every ORDER BY / LIMIT / OFFSET).
//!
//! These tests assert SQL shape only — no live database is required.
//! Locking semantics ("does this actually block a concurrent writer?")
//! belong in an integration test against a real Postgres or MySQL
//! sidecar; SQLite cannot exercise row-level locking at all because
//! it locks the entire database file per transaction.
//!
//! Coverage matrix:
//!
//! | Backend  | `lock_for_update` | `shared_lock`           |
//! |----------|-------------------|-------------------------|
//! | Postgres | `FOR UPDATE`      | `FOR SHARE`             |
//! | MySQL    | `FOR UPDATE`      | `LOCK IN SHARE MODE`    |
//! | SQLite   | (no SQL, warn!)   | (no SQL, warn!)         |

use sea_orm::DatabaseBackend;
use suprnova::{Model, model};

#[model(table = "t9_orders", timestamps = false)]
pub struct T9Order {
    pub id: i64,
    pub amount: i64,
}

// ---- lock_for_update --------------------------------------------------

#[test]
fn for_update_renders_postgres_clause() {
    let sql = T9Order::query()
        .filter("id", 1)
        .lock_for_update()
        .to_sql_for(DatabaseBackend::Postgres);
    assert!(
        sql.contains("FOR UPDATE"),
        "expected `FOR UPDATE` in Postgres SQL, got: {sql}"
    );
    // Lock clause is the last thing in the statement.
    assert!(
        sql.trim_end().ends_with("FOR UPDATE"),
        "lock clause must come after WHERE/ORDER/LIMIT, got: {sql}"
    );
}

#[test]
fn for_update_renders_mysql_clause() {
    let sql = T9Order::query()
        .filter("id", 1)
        .lock_for_update()
        .to_sql_for(DatabaseBackend::MySql);
    assert!(
        sql.contains("FOR UPDATE"),
        "expected `FOR UPDATE` in MySQL SQL, got: {sql}"
    );
    assert!(
        sql.trim_end().ends_with("FOR UPDATE"),
        "lock clause must come after WHERE/ORDER/LIMIT, got: {sql}"
    );
}

// ---- shared_lock ------------------------------------------------------

#[test]
fn shared_lock_renders_postgres_for_share() {
    let sql = T9Order::query()
        .filter("id", 1)
        .shared_lock()
        .to_sql_for(DatabaseBackend::Postgres);
    assert!(
        sql.contains("FOR SHARE"),
        "expected `FOR SHARE` in Postgres SQL, got: {sql}"
    );
    // FOR SHARE != FOR UPDATE — make sure we didn't accidentally emit the
    // wrong clause for the shared variant.
    assert!(
        !sql.contains("FOR UPDATE"),
        "shared_lock must not emit FOR UPDATE, got: {sql}"
    );
}

#[test]
fn shared_lock_renders_mysql_lock_in_share_mode() {
    let sql = T9Order::query()
        .filter("id", 1)
        .shared_lock()
        .to_sql_for(DatabaseBackend::MySql);
    assert!(
        sql.contains("LOCK IN SHARE MODE"),
        "expected `LOCK IN SHARE MODE` in MySQL SQL, got: {sql}"
    );
    assert!(
        !sql.contains("FOR UPDATE"),
        "shared_lock must not emit FOR UPDATE on MySQL, got: {sql}"
    );
}

// ---- SQLite (no-op) ---------------------------------------------------

#[test]
fn sqlite_lock_is_no_op_in_sql() {
    let sql = T9Order::query()
        .filter("id", 1)
        .lock_for_update()
        .to_sql_for(DatabaseBackend::Sqlite);
    assert!(
        !sql.contains("FOR UPDATE"),
        "SQLite must not emit FOR UPDATE, got: {sql}"
    );
    assert!(
        !sql.contains("FOR SHARE"),
        "SQLite must not emit FOR SHARE, got: {sql}"
    );
    assert!(
        !sql.contains("LOCK IN SHARE MODE"),
        "SQLite must not emit LOCK IN SHARE MODE, got: {sql}"
    );

    let shared_sql = T9Order::query()
        .filter("id", 1)
        .shared_lock()
        .to_sql_for(DatabaseBackend::Sqlite);
    assert!(
        !shared_sql.contains("FOR UPDATE"),
        "SQLite shared_lock must not emit FOR UPDATE, got: {shared_sql}"
    );
    assert!(
        !shared_sql.contains("FOR SHARE"),
        "SQLite shared_lock must not emit FOR SHARE, got: {shared_sql}"
    );
    assert!(
        !shared_sql.contains("LOCK IN SHARE MODE"),
        "SQLite shared_lock must not emit LOCK IN SHARE MODE, got: {shared_sql}"
    );
}

// ---- Default (no lock) ------------------------------------------------

#[test]
fn no_lock_method_emits_no_lock_clause() {
    // Default Builder<M> has LockMode::None — no lock clause should appear
    // regardless of backend.
    for backend in [
        DatabaseBackend::Postgres,
        DatabaseBackend::MySql,
        DatabaseBackend::Sqlite,
    ] {
        let sql = T9Order::query().filter("id", 1).to_sql_for(backend);
        assert!(
            !sql.contains("FOR UPDATE"),
            "default builder must not emit FOR UPDATE on {backend:?}, got: {sql}"
        );
        assert!(
            !sql.contains("FOR SHARE"),
            "default builder must not emit FOR SHARE on {backend:?}, got: {sql}"
        );
        assert!(
            !sql.contains("LOCK IN SHARE MODE"),
            "default builder must not emit LOCK IN SHARE MODE on {backend:?}, got: {sql}"
        );
    }
}

// ---- Lock + UNION (lock applies to outer compound) --------------------

#[test]
fn lock_appears_once_after_union_postgres() {
    // The lock clause is appended at the OUTER scope — after both
    // union arms render — so a `UNION` query has exactly one
    // `FOR UPDATE` at the very end, never one per arm. This guards
    // against regressing the union-arm-inlining behaviour in
    // `render_select_into` vs the outer wrap in `render_select_for`.
    let first = T9Order::query().filter("amount", 10);
    let second = T9Order::query().filter("amount", 20);
    let sql = first
        .union(second)
        .lock_for_update()
        .to_sql_for(DatabaseBackend::Postgres);
    assert_eq!(
        sql.matches("FOR UPDATE").count(),
        1,
        "FOR UPDATE must appear exactly once at the outer scope, got: {sql}"
    );
    assert!(
        sql.trim_end().ends_with("FOR UPDATE"),
        "lock clause must be last, got: {sql}"
    );
}
