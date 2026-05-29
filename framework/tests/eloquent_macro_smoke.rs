//! Smoke tests for the #[suprnova::model] macro core surface.
//! Each later task in Phase 10A extends this file with feature-specific
//! macro tests.

use suprnova::model;

#[model(table = "smoke_users")]
pub struct SmokeUser {
    pub id: i64,
    pub name: String,
    pub email: String,
}

#[test]
fn macro_emits_seaorm_entity() {
    // The macro should generate an `Entity` type accessible via the
    // model's own module path. SeaORM's EntityName trait gives us the
    // table name.
    let table = <smoke_user::Entity as suprnova::EntityName>::table_name(&smoke_user::Entity);
    assert_eq!(table, "smoke_users");
}

#[test]
fn macro_emits_column_enum_with_typed_variants() {
    // Column enum has one variant per non-PK field plus the PK.
    let cols = smoke_user::Column::iter().collect::<Vec<_>>();
    let names: Vec<&str> = cols.iter().map(|c| c.as_str()).collect();
    assert!(names.contains(&"id"));
    assert!(names.contains(&"name"));
    assert!(names.contains(&"email"));
    assert_eq!(names.len(), 3);
}

#[test]
fn column_from_name_round_trips() {
    let col = smoke_user::Column::from_name("name").expect("`name` is a column");
    assert_eq!(col.as_str(), "name");
    assert!(smoke_user::Column::from_name("not_a_column").is_none());
}

#[test]
fn macro_registers_model_in_inventory() {
    let entry = suprnova::find_model_by_table("smoke_users")
        .expect("registry lookup should not error")
        .expect("SmokeUser should register via #[model]");
    assert_eq!(entry.type_name, "SmokeUser");
    assert_eq!(entry.primary_key, "id");
}

#[model(
    table = "uuid_things",
    primary_key = "uid",
    key_type = "String",
    auto_increment = false
)]
pub struct UuidThing {
    pub uid: String,
    pub label: String,
}

#[test]
fn custom_primary_key_attribute() {
    let entry = suprnova::find_model_by_table("uuid_things")
        .expect("registry lookup should not error")
        .expect("registered");
    assert_eq!(entry.primary_key, "uid");
}
