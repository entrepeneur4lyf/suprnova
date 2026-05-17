//! `#[derive(Factory)]` integration test.
//!
//! Pins the auto-generated marker struct + `Factory` impl semantics:
//! the generated `<ModelName>Factory` type carries the Factory impl,
//! its `definition()` calls `Faker.fake::<Model>()` so the model's
//! `Dummy` impl drives the field generators, the visibility tracks
//! the model's, and the `#[factory(name = "...")]` override picks
//! a custom marker name.

use serial_test::serial;
use suprnova::{Dummy, Factory};

// ============================================================================
// Variant 1 — basic derive on a Dummy-deriving model
// ============================================================================

#[derive(Dummy, Factory, Debug, Clone)]
pub struct UserA {
    pub id: i32,
    pub name: String,
    pub email: String,
}

#[test]
fn factory_derive_generates_marker_struct_and_factory_impl() {
    // The generated `UserAFactory` is reachable; calling `new()` exercises
    // the generated `definition()` path, which goes through `Faker.fake::<UserA>()`
    // and therefore the model's `Dummy` impl.
    let user = UserAFactory::new().make();

    // `Dummy` for primitives picks non-default values most of the time
    // — assert a defensible "the generator actually ran" check rather
    // than a specific value.
    assert!(!user.name.is_empty(), "Dummy populated name");
    assert!(!user.email.is_empty(), "Dummy populated email");
}

#[test]
fn factory_derive_count_and_make_many_compose() {
    let users = UserAFactory::new().count(5).make_many();
    assert_eq!(users.len(), 5);
    // Independent randomness across instances.
    let distinct_names: std::collections::HashSet<_> =
        users.iter().map(|u| u.name.clone()).collect();
    assert!(
        distinct_names.len() >= 3,
        "expected mostly-distinct names across 5 fake-derived instances"
    );
}

#[test]
fn factory_derive_supports_with_overrides() {
    let user = UserAFactory::new()
        .with(|u| u.name = "Override".into())
        .make();
    assert_eq!(user.name, "Override");
}

// ============================================================================
// Variant 2 — `#[factory(name = "...")]` overrides the generated name
// ============================================================================

#[derive(Dummy, Factory, Debug)]
#[factory(name = "AdminBuilder")]
pub struct UserB {
    pub id: i32,
    pub name: String,
}

#[test]
fn factory_name_attribute_picks_custom_marker_name() {
    // No `UserBFactory` was generated — the marker is `AdminBuilder`.
    let admin = AdminBuilder::new().make();
    assert!(!admin.name.is_empty());
}

// ============================================================================
// Variant 3 — visibility propagation (pub model → pub factory)
// ============================================================================

mod inner {
    use super::{Dummy, Factory};

    // pub(crate) model → the generated factory should also be pub(crate)
    // (matches input.vis). The factory must be reachable from the test
    // function below for this to compile.
    #[derive(Dummy, Factory, Debug)]
    #[allow(dead_code)] // `id` exists to prove fielded structs work; not asserted
    pub(crate) struct Widget {
        pub id: i32,
        pub label: String,
    }
}

#[test]
fn factory_derive_propagates_model_visibility_to_marker() {
    // `inner::WidgetFactory` must be reachable here because both the
    // model `Widget` and its generated factory are pub(crate).
    let w = inner::WidgetFactory::new().make();
    assert!(!w.label.is_empty());
}

// ============================================================================
// Variant 4 — derive composes with the integration into Persistable
// (sanity check that nothing about the derive disturbs the rest of the
// factory machinery; the SeaORM-specific persistence behavior is pinned
// by framework/tests/factory_persist.rs).
// ============================================================================

#[derive(Dummy, Factory, Debug)]
pub struct WithSequence {
    pub id: i64,
    pub label: String,
}

#[test]
#[serial]
fn factory_with_sequence_assigns_monotonic_ids() {
    use suprnova::factory::Sequence;
    // Sequence overrides combine with the derived factory transparently.
    static SEQ: Sequence = Sequence::new();
    SEQ.reset();

    let items = WithSequenceFactory::new()
        .count(3)
        .with(|w| w.id = SEQ.next())
        .make_many();

    let ids: Vec<i64> = items.iter().map(|w| w.id).collect();
    assert_eq!(ids, vec![1, 2, 3], "sequence threaded into each instance");
}
