//! Regression: HIGH audit finding `eloquent` #1 — `Builder<M>` accepts
//! plain `&str` / `String` for column names, operators, and various
//! other identifier-shaped inputs through the `IntoColumn` typed-or-
//! string passthrough and through `&str` operator args. The renderer
//! interpolates these into the SQL string verbatim — values are
//! parameterised by SeaORM, but identifiers cannot be (SQL doesn't
//! allow placeholder-bound identifiers).
//!
//! For a framework whose route handlers will naturally pass
//! request-derived sort/filter/include params straight through to the
//! builder, that's an injection footgun even though the docs already
//! warn callers about the trust boundary.
//!
//! Fix: every public terminal (`get`, `count`, `first`, `exists`,
//! `paginate`, `simple_paginate`, `cursor_paginate`, the chunk family,
//! the aggregate helpers, `pluck`, `pluck_pair`) goes through one of
//! two render paths — `render_select_for` or `render_count_select_for`.
//! Both now run `Builder::validate_inputs` before emitting SQL,
//! walking every identifier and operator on the builder and rejecting
//! anything that doesn't pass `database::validate_identifier` /
//! `database::validate_sql_operator`.
//!
//! These tests prove the validator bites at the terminal boundary for
//! the surfaces that matter most — `filter` (where col), `filter_op`
//! (where col + op), `order_by_*` (sort col), `group_by` (group col),
//! `select` (projection col) — and accepts the typical legitimate
//! shapes (schema-qualified `users.id`, snake_case columns).

use suprnova::Model;
use suprnova::testing::TestDatabase;

#[suprnova::model(table = "t338_builder_ident_users")]
pub struct T338BuilderIdentUser {
    pub id: i64,
    pub email: String,
}

async fn setup() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t338_builder_ident_users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db
}

#[tokio::test]
async fn legitimate_chain_still_works() {
    let _db = setup().await;
    let _ = T338BuilderIdentUser::query()
        .filter("email", "alice@example.com")
        .filter_op("id", ">=", 1)
        .order_by_asc("email")
        .limit(10)
        .get()
        .await
        .expect("a normal identifier-shaped chain must still execute");
}

#[tokio::test]
async fn injection_in_filter_column_is_rejected_at_terminal() {
    let _db = setup().await;
    let err = T338BuilderIdentUser::query()
        .filter("id) OR (1=1", 1)
        .get()
        .await
        .expect_err("attacker-controlled where-column must be rejected at terminal");
    let msg = format!("{err}");
    assert!(
        msg.contains("SQL identifier"),
        "error must point at identifier validation; got: {msg}"
    );
}

#[tokio::test]
async fn injection_in_filter_op_operator_is_rejected() {
    let _db = setup().await;
    let err = T338BuilderIdentUser::query()
        .filter_op("id", "= 1 OR 1=1 --", 0)
        .get()
        .await
        .expect_err("attacker-controlled operator must be rejected at terminal");
    let msg = format!("{err}");
    assert!(
        msg.contains("operator"),
        "error must point at operator validation; got: {msg}"
    );
}

#[tokio::test]
async fn injection_in_order_by_column_is_rejected() {
    let _db = setup().await;
    let err = T338BuilderIdentUser::query()
        .order_by_desc("id; DROP TABLE t338_builder_ident_users")
        .get()
        .await
        .expect_err("attacker-controlled order-by column must be rejected at terminal");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn injection_in_group_by_column_is_rejected() {
    let _db = setup().await;
    let err = T338BuilderIdentUser::query()
        .group_by("id) UNION SELECT")
        .get()
        .await
        .expect_err("attacker-controlled group-by column must be rejected at terminal");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn injection_in_select_column_is_rejected() {
    let _db = setup().await;
    let err = T338BuilderIdentUser::query()
        .select(["id, (SELECT password FROM users) AS leak"])
        .get()
        .await
        .expect_err("attacker-controlled select column must be rejected at terminal");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn count_terminal_also_validates() {
    let _db = setup().await;
    let err = T338BuilderIdentUser::query()
        .filter("id) OR (1=1", 1)
        .count()
        .await
        .expect_err("count must validate the same WHERE clauses as get");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn schema_qualified_identifiers_pass() {
    // Postgres-style `schema.table` and `table.column` should pass —
    // they're a normal SeaORM/Eloquent shape for joined queries.
    let _db = setup().await;
    let _ = T338BuilderIdentUser::query()
        .filter("t338_builder_ident_users.email", "alice@example.com")
        .order_by_asc("t338_builder_ident_users.id")
        .get()
        .await
        .expect("schema-qualified identifiers must pass the validator");
}

#[suprnova::model(table = "t338_increment_counters")]
pub struct T338IncrementCounter {
    pub id: i64,
    pub views: i64,
}

#[tokio::test]
async fn increment_validates_column_argument() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t338_increment_counters (id INTEGER PRIMARY KEY AUTOINCREMENT, views INTEGER NOT NULL DEFAULT 0)",
    )
    .await
    .unwrap();

    let row = T338IncrementCounter::create(suprnova::attrs! { views: 0i64 })
        .await
        .unwrap();

    // Happy path — legitimate column name.
    row.increment("views", 1)
        .await
        .expect("a normal column name must succeed");

    // Attacker control rejected — note that `column` is interpolated
    // raw into the UPDATE statement, so the validator is the only
    // defense.
    let err = row
        .increment("views = 999 WHERE 1=1; --", 1)
        .await
        .expect_err("attacker-controlled column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));
}

// ---- Projection-column terminals -------------------------------------
//
// `pluck`, `value`, `pluck_keyed`, `sole_value`, and the SUM/AVG/MIN/MAX
// aggregates interpolate the *projection* column into the SELECT list
// directly (it is passed to the renderer as `column_expr`, bypassing
// `validate_inputs`, which only walks the WHERE / GROUP BY / ORDER BY
// / `select()` identifiers). Without an explicit guard, a
// request-derived column name reaches the SQL string verbatim — e.g.
// `query().pluck(user_param)` is an injection vector. These tests prove
// the projection column is now validated too.

#[tokio::test]
async fn pluck_projection_column_legitimate_works() {
    let _db = setup().await;
    let _: Vec<i64> = T338BuilderIdentUser::query()
        .pluck("id")
        .await
        .expect("a normal projection column must still execute");
}

#[tokio::test]
async fn pluck_projection_column_injection_is_rejected() {
    let _db = setup().await;
    let err = T338BuilderIdentUser::query()
        .pluck::<i64>("id; DROP TABLE t338_builder_ident_users--")
        .await
        .expect_err("attacker-controlled pluck column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn pluck_subquery_projection_is_rejected() {
    let _db = setup().await;
    let err = T338BuilderIdentUser::query()
        .pluck::<i64>("id, (SELECT email FROM t338_builder_ident_users)")
        .await
        .expect_err("a subquery smuggled into the pluck column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn value_projection_column_injection_is_rejected() {
    let _db = setup().await;
    let err = T338BuilderIdentUser::query()
        .value::<i64>("id) UNION SELECT email FROM t338_builder_ident_users --")
        .await
        .expect_err("attacker-controlled value column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn pluck_keyed_projection_columns_are_validated() {
    let _db = setup().await;

    // Malicious key column.
    let err = T338BuilderIdentUser::query()
        .pluck_keyed::<i64, String>("id) OR (1=1", "email")
        .await
        .expect_err("attacker-controlled pluck_keyed key column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));

    // Malicious value column.
    let err = T338BuilderIdentUser::query()
        .pluck_keyed::<i64, String>("id", "email FROM t338_builder_ident_users --")
        .await
        .expect_err("attacker-controlled pluck_keyed value column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));

    // Both legitimate — must still execute.
    let _: std::collections::HashMap<i64, String> = T338BuilderIdentUser::query()
        .pluck_keyed("id", "email")
        .await
        .expect("legitimate key/value columns must still execute");
}

#[tokio::test]
async fn aggregate_projection_columns_are_validated() {
    let _db = setup().await;

    let err = T338BuilderIdentUser::query()
        .sum::<i64>("id) FROM t338_builder_ident_users --")
        .await
        .expect_err("attacker-controlled SUM column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));

    let err = T338BuilderIdentUser::query()
        .avg::<f64>("id); DROP TABLE t338_builder_ident_users --")
        .await
        .expect_err("attacker-controlled AVG column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));

    let err = T338BuilderIdentUser::query()
        .min::<i64>("id) UNION SELECT email")
        .await
        .expect_err("attacker-controlled MIN column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));

    let err = T338BuilderIdentUser::query()
        .max::<i64>("id, (SELECT email)")
        .await
        .expect_err("attacker-controlled MAX column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));

    // Legitimate aggregates must still execute.
    let _: i64 = T338BuilderIdentUser::query()
        .sum("id")
        .await
        .expect("a normal SUM column must still execute");
}
