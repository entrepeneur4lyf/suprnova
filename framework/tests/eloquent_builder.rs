//! Phase 10A T5 — Builder<M> dual-API + full where surface.
//!
//! Asserts every where-shape method has both Rust-shape and Laravel-
//! shape aliases that produce identical SQL. Each test sets up an
//! in-memory SQLite database, creates an ad-hoc `t5_users` table, and
//! exercises one slice of the surface. The macro emits an inner
//! module `t5_user` whose `Column` enum is reachable as
//! `t5_user::Column::Email`.

use chrono::NaiveDate;
use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, Direction, Model};

#[model(table = "t5_users", timestamps = false)]
pub struct T5User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub age: i32,
    pub active: bool,
    pub role: String,
    pub balance: f64,
    pub bio: Option<String>,
    pub birthday: Option<NaiveDate>,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        r#"CREATE TABLE t5_users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            email TEXT NOT NULL UNIQUE,
            age INTEGER NOT NULL,
            active INTEGER NOT NULL,
            role TEXT NOT NULL,
            balance REAL NOT NULL,
            bio TEXT,
            birthday TEXT
        )"#,
    )
    .await
    .expect("create table");
}

// to_sql() reads from DB::connection().ok(), so without a live database
// it falls back to Sqlite dialect. Use a fresh in-memory DB per test so
// the placeholder shape is deterministic.

#[tokio::test]
async fn filter_and_db_where_produce_identical_sql() {
    let _db = TestDatabase::sqlite_memory().await.expect("sqlite");
    let rust_sql = T5User::query()
        .filter("email", "alice@example.com")
        .filter("active", true)
        .to_sql();
    let laravel_sql = T5User::query()
        .db_where("email", "alice@example.com")
        .db_where("active", true)
        .to_sql();
    assert_eq!(rust_sql, laravel_sql);
}

#[tokio::test]
async fn filter_op_handles_arbitrary_operator() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "A", email: "a@x.com", age: 17, active: true, role: "u", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "B", email: "b@x.com", age: 25, active: true, role: "u", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "C", email: "c@x.com", age: 30, active: true, role: "u", balance: 0.0)).await.unwrap();

    let adults = T5User::query().filter_op("age", ">=", 18).get().await.unwrap();
    assert_eq!(adults.len(), 2);

    let same = T5User::query().db_where_op("age", ">=", 18).get().await.unwrap();
    assert_eq!(same.len(), 2);
}

#[tokio::test]
async fn filter_in_and_where_in_match() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "A", email: "a@x.com", age: 20, active: true, role: "admin", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "B", email: "b@x.com", age: 20, active: true, role: "user", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "C", email: "c@x.com", age: 20, active: true, role: "moderator", balance: 0.0)).await.unwrap();

    let admins_or_mods = T5User::query().filter_in("role", ["admin", "moderator"]).get().await.unwrap();
    assert_eq!(admins_or_mods.len(), 2);

    let same = T5User::query().where_in("role", ["admin", "moderator"]).get().await.unwrap();
    assert_eq!(same.len(), 2);
}

#[tokio::test]
async fn filter_null_and_filter_not_null() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "A", email: "a@x.com", age: 20, active: true, role: "u", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "B", email: "b@x.com", age: 20, active: true, role: "u", balance: 0.0, bio: "Hi")).await.unwrap();

    let no_bio = T5User::query().filter_null("bio").get().await.unwrap();
    assert_eq!(no_bio.len(), 1);
    let has_bio = T5User::query().where_not_null("bio").get().await.unwrap();
    assert_eq!(has_bio.len(), 1);
}

#[tokio::test]
async fn filter_between_inclusive_range() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    for age in [10, 18, 25, 30, 65] {
        T5User::create(attrs!(name: "X", email: format!("e{age}@x.com"), age: age, active: true, role: "u", balance: 0.0)).await.unwrap();
    }
    let working_age = T5User::query().filter_between("age", 18..=64).get().await.unwrap();
    assert_eq!(working_age.len(), 3);

    let same = T5User::query().where_between("age", 18..=64).get().await.unwrap();
    assert_eq!(same.len(), 3);
}

#[tokio::test]
async fn filter_not_between_inverse_range() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    for age in [10, 18, 25, 30, 65] {
        T5User::create(attrs!(name: "X", email: format!("e{age}@x.com"), age: age, active: true, role: "u", balance: 0.0)).await.unwrap();
    }
    let outside = T5User::query().filter_not_between("age", 18..=64).get().await.unwrap();
    assert_eq!(outside.len(), 2);

    let same = T5User::query().where_not_between("age", 18..=64).get().await.unwrap();
    assert_eq!(same.len(), 2);
}

#[tokio::test]
async fn filter_like_substring_match() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "Alice", email: "a@example.com", age: 20, active: true, role: "u", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "Bob",   email: "b@other.com",   age: 20, active: true, role: "u", balance: 0.0)).await.unwrap();

    let example_users = T5User::query().filter_like("email", "%@example.com").get().await.unwrap();
    assert_eq!(example_users.len(), 1);
    assert_eq!(example_users[0].name, "Alice");

    let same = T5User::query().where_like("email", "%@example.com").get().await.unwrap();
    assert_eq!(same.len(), 1);
}

#[tokio::test]
async fn filter_not_like_inverse_substring() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "Alice", email: "a@example.com", age: 20, active: true, role: "u", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "Bob",   email: "b@other.com",   age: 20, active: true, role: "u", balance: 0.0)).await.unwrap();

    let non_example = T5User::query().filter_not_like("email", "%@example.com").get().await.unwrap();
    assert_eq!(non_example.len(), 1);
    assert_eq!(non_example[0].name, "Bob");

    let same = T5User::query().where_not_like("email", "%@example.com").get().await.unwrap();
    assert_eq!(same.len(), 1);
}

#[tokio::test]
async fn or_filter_short_circuits_correctly() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "Alice", email: "a@x.com", age: 18, active: false, role: "admin", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "Bob",   email: "b@x.com", age: 21, active: true, role: "user",  balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "Eve",   email: "e@x.com", age: 30, active: false, role: "user",  balance: 0.0)).await.unwrap();

    let active_or_admins = T5User::query()
        .filter("active", true)
        .or_filter("role", "admin")
        .get()
        .await
        .unwrap();
    assert_eq!(active_or_admins.len(), 2);

    let same = T5User::query()
        .filter("active", true)
        .or_where("role", "admin")
        .get()
        .await
        .unwrap();
    assert_eq!(same.len(), 2);
}

#[tokio::test]
async fn order_by_asc_desc_random_raw() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    for (name, age) in [("A", 10), ("B", 20), ("C", 30)] {
        T5User::create(attrs!(name: name, email: format!("{name}@x.com"), age: age, active: true, role: "u", balance: 0.0)).await.unwrap();
    }
    let asc = T5User::query().order_by("age", Direction::Asc).get().await.unwrap();
    assert_eq!(asc[0].name, "A");
    let desc = T5User::query().order_by_desc("age").get().await.unwrap();
    assert_eq!(desc[0].name, "C");
    let by_raw = T5User::query().order_by_raw("age * -1").get().await.unwrap();
    assert_eq!(by_raw[0].name, "C");
    // in_random_order — verify it runs without panic:
    let _ = T5User::query().in_random_order().get().await.unwrap();
}

#[tokio::test]
async fn aggregates_count_sum_avg_min_max() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    for amt in [10.0_f64, 20.0, 30.0] {
        T5User::create(attrs!(name: "X", email: format!("x{amt}@x.com"), age: 20, active: true, role: "u", balance: amt)).await.unwrap();
    }
    assert_eq!(T5User::count().await.unwrap(), 3);
    assert!((T5User::sum::<f64>("balance").await.unwrap() - 60.0).abs() < 1e-6);
    assert!((T5User::avg::<f64>("balance").await.unwrap() - 20.0).abs() < 1e-6);
    assert_eq!(T5User::min::<f64>("balance").await.unwrap(), Some(10.0));
    assert_eq!(T5User::max::<f64>("balance").await.unwrap(), Some(30.0));
}

#[tokio::test]
async fn exists_and_doesnt_exist() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    assert!(!T5User::query().filter("email", "a@x.com").exists().await.unwrap());
    assert!(T5User::query().filter("email", "a@x.com").doesnt_exist().await.unwrap());
    T5User::create(attrs!(name: "A", email: "a@x.com", age: 20, active: true, role: "u", balance: 0.0)).await.unwrap();
    assert!(T5User::query().filter("email", "a@x.com").exists().await.unwrap());
}

#[tokio::test]
async fn typed_column_works_alongside_string() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "Alice", email: "a@x.com", age: 20, active: true, role: "u", balance: 0.0)).await.unwrap();

    let by_string = T5User::query().filter("email", "a@x.com").first().await.unwrap();
    let by_typed = T5User::query().filter(t5_user::Column::Email, "a@x.com").first().await.unwrap();

    assert!(by_string.is_some());
    assert!(by_typed.is_some());
    assert_eq!(by_string.unwrap().id, by_typed.unwrap().id);
}

#[tokio::test]
async fn limit_offset_and_take_skip_aliases() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    for i in 0..5 {
        T5User::create(attrs!(name: "X", email: format!("e{i}@x.com"), age: 20, active: true, role: "u", balance: 0.0)).await.unwrap();
    }
    let page_1 = T5User::query().order_by_asc("id").limit(2).offset(0).get().await.unwrap();
    let page_2 = T5User::query().order_by_asc("id").take(2).skip(2).get().await.unwrap();
    assert_eq!(page_1.len(), 2);
    assert_eq!(page_2.len(), 2);
    assert_ne!(page_1[0].id, page_2[0].id);
}

#[tokio::test]
async fn pluck_and_pluck_keyed() {
    use std::collections::HashMap;
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "A", email: "a@x.com", age: 20, active: true, role: "u", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "B", email: "b@x.com", age: 20, active: true, role: "u", balance: 0.0)).await.unwrap();

    let emails: Vec<String> = T5User::pluck::<String>("email").await.unwrap();
    assert_eq!(emails.len(), 2);
    let keyed: HashMap<i64, String> = T5User::pluck_keyed::<i64, String>("id", "name").await.unwrap();
    assert_eq!(keyed.len(), 2);
}

#[tokio::test]
async fn distinct_dedup() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "A", email: "a@x.com", age: 20, active: true, role: "admin", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "B", email: "b@x.com", age: 20, active: true, role: "admin", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "C", email: "c@x.com", age: 20, active: true, role: "user", balance: 0.0)).await.unwrap();
    let roles: Vec<String> = T5User::query().distinct().pluck("role").await.unwrap();
    assert_eq!(roles.len(), 2);
}

#[tokio::test]
async fn to_sql_renders_parameterised() {
    // Without a live connection, to_sql falls back to Sqlite dialect.
    let sql = T5User::query()
        .filter("active", true)
        .filter_op("age", ">=", 18)
        .order_by_desc("created_at")
        .limit(10)
        .to_sql();
    assert!(sql.starts_with("SELECT"));
    assert!(sql.contains("WHERE"));
    assert!(sql.contains("?"));
    assert!(sql.contains("ORDER BY"));
    assert!(sql.contains("LIMIT"));
}

#[tokio::test]
async fn to_sql_for_postgres_emits_dollar_placeholders() {
    use sea_orm::DbBackend;
    let sql = T5User::query()
        .filter("active", true)
        .filter_op("age", ">=", 18)
        .to_sql_for(DbBackend::Postgres);
    assert!(sql.contains("$1"), "expected $N placeholders for Postgres, got: {sql}");
    assert!(sql.contains("$2"));
    assert!(!sql.contains(" ? "), "Postgres rendering should not contain `?` placeholders");
}

#[tokio::test]
async fn to_delete_sql_renders_delete_from_where() {
    use sea_orm::DbBackend;
    let (sql, vals) = T5User::query()
        .filter("active", false)
        .filter_op("age", "<", 18)
        .to_delete_sql_with_bindings_for(DbBackend::Sqlite, "t5_users");
    assert_eq!(vals.len(), 2);
    assert!(sql.starts_with("DELETE FROM t5_users"));
    assert!(sql.contains("WHERE"));
    assert!(sql.contains("active = ?"));
    assert!(sql.contains("age < ?"));
    // No SELECT, no ORDER BY, no LIMIT — DELETE form only.
    assert!(!sql.contains("SELECT"));
    assert!(!sql.contains("ORDER BY"));
    assert!(!sql.contains("LIMIT"));
}

#[tokio::test]
async fn filter_not_and_filter_not_in() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "A", email: "a@x.com", age: 20, active: true, role: "admin", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "B", email: "b@x.com", age: 20, active: true, role: "user", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "C", email: "c@x.com", age: 20, active: true, role: "guest", balance: 0.0)).await.unwrap();

    let not_admins = T5User::query().filter_not("role", "admin").get().await.unwrap();
    assert_eq!(not_admins.len(), 2);
    let not_admins_alias = T5User::query().where_not("role", "admin").get().await.unwrap();
    assert_eq!(not_admins_alias.len(), 2);

    let not_user_or_guest = T5User::query().filter_not_in("role", ["user", "guest"]).get().await.unwrap();
    assert_eq!(not_user_or_guest.len(), 1);
    let same = T5User::query().where_not_in("role", ["user", "guest"]).get().await.unwrap();
    assert_eq!(same.len(), 1);
}

#[tokio::test]
async fn filter_date_year_month_day() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "A", email: "a@x.com", age: 20, active: true, role: "u", balance: 0.0, birthday: "1990-05-15")).await.unwrap();
    T5User::create(attrs!(name: "B", email: "b@x.com", age: 20, active: true, role: "u", balance: 0.0, birthday: "1985-05-15")).await.unwrap();

    let by_year = T5User::query().filter_year("birthday", 1990).get().await.unwrap();
    assert_eq!(by_year.len(), 1);
    let by_year_alias = T5User::query().where_year("birthday", 1990).get().await.unwrap();
    assert_eq!(by_year_alias.len(), 1);

    let by_month = T5User::query().filter_month("birthday", 5).get().await.unwrap();
    assert_eq!(by_month.len(), 2);
    let by_day = T5User::query().filter_day("birthday", 15).get().await.unwrap();
    assert_eq!(by_day.len(), 2);

    let by_exact = T5User::query().filter_date("birthday", NaiveDate::from_ymd_opt(1990, 5, 15).unwrap()).get().await.unwrap();
    assert_eq!(by_exact.len(), 1);
}

#[tokio::test]
async fn filter_column_and_filter_raw() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "A", email: "a@x.com", age: 20, active: true, role: "u", balance: 0.0)).await.unwrap();

    let by_raw = T5User::query().filter_raw("age = ?", vec![serde_json::json!(20)]).get().await.unwrap();
    assert_eq!(by_raw.len(), 1);
    let same = T5User::query().where_raw("age = ?", vec![serde_json::json!(20)]).get().await.unwrap();
    assert_eq!(same.len(), 1);

    // filter_column compares two columns (always equal in our setup since
    // there's only one row; verify it runs without error).
    let by_col = T5User::query().filter_column("age", "age").get().await.unwrap();
    assert_eq!(by_col.len(), 1);
    let same_col = T5User::query().where_column("age", "age").get().await.unwrap();
    assert_eq!(same_col.len(), 1);
}

#[tokio::test]
async fn union_postgres_placeholders_are_monotonic() {
    // Regression: render_select_for used to reset `n` on each recursive
    // union call, so the second SELECT's $N restarted at $1 — colliding
    // with the outer SELECT's bound parameters. The shared counter
    // version threads `n` through, yielding $1, $2, $3, $4 across the
    // combined statement.
    use sea_orm::DbBackend;
    let first = T5User::query().filter("active", true).filter_op("age", ">=", 18);
    let second = T5User::query().filter("role", "admin").filter_op("age", "<", 65);
    let (sql, vals) = first.union(second).to_sql_with_bindings_for(DbBackend::Postgres);
    assert_eq!(vals.len(), 4);
    assert!(sql.contains("$1"), "got: {sql}");
    assert!(sql.contains("$2"), "got: {sql}");
    assert!(sql.contains("$3"), "got: {sql}");
    assert!(sql.contains("$4"), "got: {sql}");
    // No `$1` placeholder should appear twice — that would mean the
    // inner SELECT restarted numbering. Count occurrences as a whole
    // word (i.e., not as part of `$10`, `$11`, ...).
    let count_dollar_one = sql.matches(" $1 ").count()
        + sql.matches("=$1").count()
        + sql.matches(",$1").count();
    assert!(count_dollar_one <= 1, "$1 appeared >1 times: {sql}");
}

#[tokio::test]
async fn union_combines_two_queries() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    T5User::create(attrs!(name: "Active", email: "a@x.com", age: 20, active: true, role: "user", balance: 0.0)).await.unwrap();
    T5User::create(attrs!(name: "Admin", email: "b@x.com", age: 20, active: false, role: "admin", balance: 0.0)).await.unwrap();

    let first = T5User::query().filter("active", true);
    let second = T5User::query().filter("role", "admin");
    let users = first.union(second).get().await.unwrap();
    assert_eq!(users.len(), 2);
}
