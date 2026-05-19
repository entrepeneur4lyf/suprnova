//! Phase 10A T8 — `#[accessor]` and `#[mutator]` function-level macros.
//!
//! Each model is declared at module scope (NOT inside the test fns).
//! `#[suprnova::model]` emits an inner module whose `use super::*;`
//! only sees the test file's top-level imports — putting models inside
//! `#[tokio::test]` fns breaks SeaORM type resolution. See
//! `eloquent_casts_primitive.rs` for the same constraint.

use suprnova::eloquent::FirstOrCreate;
use suprnova::testing::TestDatabase;
use suprnova::{accessor, attrs, model, mutator, Model};

// ---- Models -------------------------------------------------------------

#[model(
    table = "t8_users",
    timestamps = false,
    appends = ["full_name"],
    fillable = ["first_name", "last_name", "password"],
    mutators = ["password"]
)]
pub struct T8User {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub password: String,
}

impl T8User {
    #[accessor]
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }

    /// Mutator. Receives a `serde_json::Value`; the body deserialises
    /// it into whatever runtime type the field needs and applies any
    /// transformation (here: prepend `"hashed:"` so we can observe
    /// the mutator fired).
    #[mutator]
    pub fn set_password(
        &mut self,
        value: serde_json::Value,
    ) -> Result<(), suprnova::FrameworkError> {
        let raw: String = serde_json::from_value(value).map_err(|e| {
            suprnova::FrameworkError::validation("password", format!("{e}"))
        })?;
        self.password = format!("hashed:{raw}");
        Ok(())
    }
}

#[model(
    table = "t8_hidden",
    timestamps = false,
    appends = [],
    hidden = ["secret"],
    fillable = ["name", "secret"]
)]
pub struct T8Hidden {
    pub id: i64,
    pub name: String,
    pub secret: String,
}

#[model(
    table = "t8_visible",
    timestamps = false,
    visible = ["id", "name"],
    fillable = ["name", "secret"]
)]
pub struct T8Visible {
    pub id: i64,
    pub name: String,
    pub secret: String,
}

#[model(
    table = "t8_counters",
    timestamps = false,
    fillable = ["clicks"],
    mutators = ["clicks"]
)]
pub struct T8Counter {
    pub id: i64,
    pub clicks: i32,
}

impl T8Counter {
    #[mutator]
    pub fn set_clicks(
        &mut self,
        value: serde_json::Value,
    ) -> Result<(), suprnova::FrameworkError> {
        let raw: i32 = serde_json::from_value(value).map_err(|e| {
            suprnova::FrameworkError::validation("clicks", format!("{e}"))
        })?;
        // Mutator transformation: double the incoming value so we can
        // observe the mutator fired even for an i32 field.
        self.clicks = raw * 2;
        Ok(())
    }
}

// Exercises both appends and hidden on the same model — hidden
// strips fields from the base map, then appends inserts accessor
// outputs afterwards.
#[model(
    table = "t8_appends_hidden",
    timestamps = false,
    appends = ["full_name"],
    hidden = ["password"],
    fillable = ["first_name", "last_name", "password"]
)]
pub struct T8AppendsHidden {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub password: String,
}

impl T8AppendsHidden {
    #[accessor]
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }
}

// ---- Migrations ---------------------------------------------------------

async fn migrate_users(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE t8_users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            first_name TEXT NOT NULL,
            last_name TEXT NOT NULL,
            password TEXT NOT NULL
        )",
    )
    .await
    .expect("create t8_users");
}

async fn migrate_counters(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE t8_counters (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            clicks INTEGER NOT NULL
        )",
    )
    .await
    .expect("create t8_counters");
}

// ---- Tests --------------------------------------------------------------

#[tokio::test]
async fn accessor_callable_directly() {
    let u = T8User {
        id: 0,
        first_name: "Alice".into(),
        last_name: "X".into(),
        password: "".into(),
    };
    assert_eq!(u.full_name(), "Alice X");
}

#[tokio::test]
async fn accessor_appended_in_to_json() {
    let u = T8User {
        id: 1,
        first_name: "Alice".into(),
        last_name: "X".into(),
        password: "".into(),
    };
    let v = u.to_json();
    assert_eq!(v["full_name"], "Alice X");
    assert_eq!(v["first_name"], "Alice");
}

#[tokio::test]
async fn mutator_runs_via_fill_path() {
    let _db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate_users(&_db).await;

    let u = T8User::create(attrs! {
        first_name: "Alice",
        last_name: "X",
        password: "plain",
    })
    .await
    .expect("create T8User");
    assert_eq!(u.password, "hashed:plain");
}

#[tokio::test]
async fn direct_field_assignment_skips_mutator() {
    let mut u = T8User {
        id: 0,
        first_name: "A".into(),
        last_name: "B".into(),
        password: "".into(),
    };
    u.password = "raw".into();
    assert_eq!(u.password, "raw");
}

#[tokio::test]
async fn hidden_fields_excluded_from_to_json() {
    let u = T8Hidden {
        id: 1,
        name: "Alice".into(),
        secret: "shh".into(),
    };
    let v = u.to_json();
    assert_eq!(v["name"], "Alice");
    assert!(
        v.get("secret").is_none(),
        "hidden field should not serialise"
    );
}

#[tokio::test]
async fn visible_allowlist_drops_non_listed_fields() {
    // `visible = ["id", "name"]` keeps only id + name; secret is
    // dropped even though it isn't listed in `hidden`.
    let u = T8Visible {
        id: 7,
        name: "Alice".into(),
        secret: "shh".into(),
    };
    let v = u.to_json();
    assert_eq!(v["id"], 7);
    assert_eq!(v["name"], "Alice");
    assert!(
        v.get("secret").is_none(),
        "secret should be excluded by visible-allowlist"
    );
}

#[tokio::test]
async fn mutator_works_with_non_string_typed_fields() {
    // Proves the value-style mutator contract works for any runtime
    // type — i32 here. The macro never inspects the setter signature;
    // the user's body owns the deserialise step.
    let _db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate_counters(&_db).await;

    let c = T8Counter::create(attrs! { clicks: 5 })
        .await
        .expect("create T8Counter");
    assert_eq!(c.clicks, 10, "mutator should have doubled the value");
}

#[tokio::test]
async fn first_or_new_routes_through_mutator() {
    // Closes the latent gap noted in the T8 brief: prior to T8,
    // `from_attrs_unsaved` (called by `first_or_new`) did direct
    // field assignment and skipped mutators. T8 routes mutator-listed
    // fields through `s.set_<field>(value)?` so unsaved builders
    // apply the same transformation as `create` / `update`.
    let _db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate_users(&_db).await;

    let u = T8User::first_or_new(attrs! {
        first_name: "Alice",
        last_name: "X",
        password: "plain",
    })
    .await
    .expect("first_or_new");
    assert_eq!(
        u.password, "hashed:plain",
        "first_or_new should route through the password mutator"
    );
}

#[tokio::test]
async fn direct_fill_call_routes_through_mutator_and_filter() {
    // `fill` is the public in-memory transform entry point. Direct
    // calls should honour both the mutator routing and the
    // mass-assignment filter.
    let mut u = T8User::default();
    u.fill(attrs! {
        first_name: "Alice",
        last_name: "X",
        password: "plain",
    })
    .expect("fill");
    assert_eq!(u.first_name, "Alice");
    assert_eq!(u.last_name, "X");
    assert_eq!(
        u.password, "hashed:plain",
        "fill should route password through the mutator"
    );
}

#[tokio::test]
async fn appends_coexist_with_hidden_filter() {
    // appends + hidden coexist: hidden drops fields from the base
    // serialization, appends inserts accessor outputs afterwards.
    // T8AppendsHidden (module-scope, below) declares both.
    let u = T8AppendsHidden {
        id: 1,
        first_name: "Alice".into(),
        last_name: "X".into(),
        password: "secret".into(),
    };
    let v = u.to_json();
    assert_eq!(v["first_name"], "Alice");
    assert_eq!(v["full_name"], "Alice X");
    assert!(
        v.get("password").is_none(),
        "hidden field should not serialise"
    );
}
