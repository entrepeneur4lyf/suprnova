//! Phase 10B T1 — verifies that `#[suprnova::model(relations = { ... })]`
//! emits an `inventory::submit!(RelationEntry { ... })` per declared
//! relation, and that the helpers (`relations`, `relations_of`,
//! `find_relation`) surface those entries by type / name.
//!
//! T1 only exercises the inventory path; the per-relation method
//! bodies (`fn profile(&self) -> HasOne<Self, RegProfile>`) land in
//! T2 once `HasOne` exists. Declaring `relations = { profile:
//! HasOne<RegProfile> }` is legal in T1 because the macro doesn't
//! emit a relation method yet — only the accessors that read from
//! `__eager` plus the inventory submission.

use suprnova::{find_relation, model, relations, relations_of, RelationKind};

#[model(table = "reg_users", relations = {
    profile: HasOne<RegProfile>,
    posts: HasMany<RegPost>,
})]
pub struct RegUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "reg_profiles")]
pub struct RegProfile {
    pub id: i64,
    pub bio: String,
}

#[model(table = "reg_posts")]
pub struct RegPost {
    pub id: i64,
    pub user_id: i64,
    pub title: String,
}

#[test]
fn relation_registered_in_inventory() {
    let entry = relations()
        .find(|r| r.parent_type_name == "RegUser" && r.name == "profile")
        .expect("relation profile should be registered");
    assert_eq!(entry.kind, RelationKind::HasOne);
    // `target_type_name` is the type literal as written in the
    // macro body — `RegProfile`, not the full module path. Tooling
    // (Phase 8) displays this in admin UIs.
    assert_eq!(entry.target_type_name, "RegProfile");
}

#[test]
fn multiple_relations_on_one_model_each_register() {
    // Both `profile` and `posts` should appear independently.
    let names: Vec<&str> = relations()
        .filter(|r| r.parent_type_name == "RegUser")
        .map(|r| r.name)
        .collect();
    assert!(
        names.contains(&"profile"),
        "expected profile registered, got: {names:?}",
    );
    assert!(
        names.contains(&"posts"),
        "expected posts registered, got: {names:?}",
    );
}

#[test]
fn relations_of_filters_by_parent_type() {
    // `relations_of::<T>()` returns only entries whose parent_type
    // matches — used by Phase 8 (Admin) to walk a single model's
    // relation surface.
    let count = relations_of::<RegUser>().count();
    assert!(
        count >= 2,
        "expected at least 2 relations on RegUser, got {count}",
    );
    // Every entry's parent_type_name must be RegUser.
    for entry in relations_of::<RegUser>() {
        assert_eq!(entry.parent_type_name, "RegUser");
    }
}

#[test]
fn find_relation_resolves_by_name() {
    let entry =
        find_relation::<RegUser>("posts").expect("RegUser::posts must be findable by name");
    assert_eq!(entry.name, "posts");
    assert_eq!(entry.kind, RelationKind::HasMany);
    assert_eq!(entry.target_type_name, "RegPost");
}

#[test]
fn find_relation_returns_none_for_unknown_name() {
    let entry = find_relation::<RegUser>("nope");
    assert!(entry.is_none(), "unknown relation name must resolve to None");
}

#[test]
fn relations_iter_includes_every_declaration() {
    // The global iterator must include both entries declared above
    // (alongside any other relations declared elsewhere in the test
    // binary — order is link-time-arbitrary).
    let any_profile = relations()
        .any(|r| r.parent_type_name == "RegUser" && r.name == "profile");
    let any_posts = relations()
        .any(|r| r.parent_type_name == "RegUser" && r.name == "posts");
    assert!(any_profile, "global relations() must surface RegUser::profile");
    assert!(any_posts, "global relations() must surface RegUser::posts");
}
