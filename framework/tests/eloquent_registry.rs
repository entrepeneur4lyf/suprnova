//! Locks the `suprnova::eloquent` registry surface.
//!
//! Phase 10A foundation. Verifies that `inventory::submit!(ModelEntry
//! { ... })` is the registration mechanism downstream tasks (T3 macro)
//! and downstream phases (8 Admin, future `model:prune`) will use to
//! enumerate every `#[suprnova::model]`-decorated struct in the
//! binary.
//!
//! Fixture entries below register synthetic models so the test binary
//! has known data to walk. Naming uses a `TestRegistry` prefix to
//! avoid colliding with any future real `#[suprnova::model]`
//! registrations (which would also flow through this same registry).

use suprnova::eloquent::{ModelEntry, models};

inventory::submit! {
    ModelEntry {
        type_name: "TestRegistryUser",
        table: "test_registry_users",
        module_path: "framework::tests::eloquent_registry",
        primary_key: "id",
    }
}

inventory::submit! {
    ModelEntry {
        type_name: "TestRegistryPost",
        table: "test_registry_posts",
        module_path: "framework::tests::eloquent_registry",
        primary_key: "id",
    }
}

#[test]
fn models_iter_yields_registered_entries() {
    let names: Vec<&'static str> = models().map(|m| m.type_name).collect();
    assert!(
        names.contains(&"TestRegistryUser"),
        "missing TestRegistryUser, got {names:?}"
    );
    assert!(
        names.contains(&"TestRegistryPost"),
        "missing TestRegistryPost, got {names:?}"
    );
}

#[test]
fn model_entry_fields_round_trip() {
    let user = models()
        .find(|m| m.type_name == "TestRegistryUser")
        .expect("TestRegistryUser should be registered");
    assert_eq!(user.table, "test_registry_users");
    assert_eq!(user.primary_key, "id");
    assert!(user.module_path.contains("eloquent_registry"));
}

#[test]
fn lookup_by_table_finds_entry() {
    let entry = suprnova::eloquent::find_model_by_table("test_registry_posts");
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().type_name, "TestRegistryPost");
    assert!(suprnova::eloquent::find_model_by_table("nope").is_none());
}
