//! Phase 10B Task 1 — verifies the `Relation` trait shape + the
//! `RelationKind` enum covers every Eloquent relation flavour up-front.
//!
//! Concrete relation types (`HasOne`, `HasMany`, ...) land in T2-T7
//! and replace the dummy used here.

use suprnova::{Relation, RelationKind};

// A no-op stand-in to verify trait shape compiles. Concrete relation
// types (HasOne, HasMany, ...) land in T2-T7 and replace this.
struct DummyRelation;

impl Relation for DummyRelation {
    type Parent = ();
    type Target = ();
    const KIND: RelationKind = RelationKind::HasOne;
    fn parent_key(&self) -> &str {
        "id"
    }
    fn foreign_key(&self) -> &str {
        "parent_id"
    }
}

#[test]
fn relation_trait_dispatches_to_kind() {
    let d = DummyRelation;
    assert_eq!(<DummyRelation as Relation>::KIND, RelationKind::HasOne);
    assert_eq!(d.parent_key(), "id");
    assert_eq!(d.foreign_key(), "parent_id");
}

#[test]
fn relation_kind_covers_every_eloquent_relation() {
    // T1 enumerates the full set up-front so T2-T7 only add concrete
    // impls — they don't extend the enum.
    let _ = RelationKind::HasOne;
    let _ = RelationKind::BelongsTo;
    let _ = RelationKind::HasMany;
    let _ = RelationKind::BelongsToMany;
    let _ = RelationKind::HasOneThrough;
    let _ = RelationKind::HasManyThrough;
    let _ = RelationKind::MorphTo;
    let _ = RelationKind::MorphOne;
    let _ = RelationKind::MorphMany;
    let _ = RelationKind::MorphToMany;
    let _ = RelationKind::MorphedByMany;
}

#[test]
fn aggregate_kind_covers_every_eloquent_aggregate() {
    // `with_sum` / `with_avg` / `with_min` / `with_max` are the four
    // aggregate eager-load methods. T1 enumerates them so T9 (eager
    // loading) and T3+ (HasMany / BelongsToMany / Through / Morph m2m)
    // only add match arms.
    use suprnova::AggregateKind;
    let _ = AggregateKind::Sum;
    let _ = AggregateKind::Avg;
    let _ = AggregateKind::Min;
    let _ = AggregateKind::Max;
}
