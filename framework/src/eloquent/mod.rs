//! Eloquent — Laravel-shape API layered over SeaORM.
//!
//! See `docs/core/eloquent.md` for the user guide and
//! `docs/superpowers/specs/phase-10/` for the design contract.
//!
//! Phase 10A ships the foundation: `#[suprnova::model]` macro,
//! `Model` trait (CRUD lifecycle), `Builder<M>` (dual-API where
//! surface), Fillable/Guarded, the 21 built-in casts, accessors and
//! mutators, auto-managed timestamps, and soft deletes + Prunable.
//! Phase 10B adds relationships; Phase 10C adds collections /
//! pagination / observers / transactions / multi-connection.

pub mod attrs;
pub mod builder;
pub mod casts;
pub mod collection;
pub mod console;
pub mod events;
pub mod fillable;
pub mod lazy;
pub mod model;
pub mod observers;
pub mod prunable;
pub mod registry;
pub mod relations;
pub mod scopes;
pub mod soft_deletes;
pub mod timestamps;
pub mod unique_id;

pub use attrs::Attrs;
pub use builder::{Builder, Direction, IntoColumn, IntoVal};
pub use casts::{
    AsArray, AsArrayObject, AsBool, AsCollection, AsDate, AsDateTime, AsDecimal, AsEncrypted,
    AsEncryptedArray, AsEncryptedCollection, AsEncryptedObject, AsEnum, AsFloat, AsHashed,
    AsImmutableDate, AsImmutableDateTime, AsInt, AsJson, AsObject, AsOptionalDateTime, AsString,
    AsTimestamp, Cast, DynCast, IntoDynCast,
};
pub use collection::Collection;
pub use fillable::{
    Fillable, prevent_silently_discarding_attributes, preventing_silently_discarding_attributes,
    unguarded,
};
pub use lazy::LazyCollection;
pub use model::{FirstOrCreate, Model, ReplicateExt};
pub use prunable::{
    MassPrunable, Prunable, PrunerEntry, PrunerFn, prune_all, prune_all_dry, prune_one, pruners,
};
pub use registry::{ModelEntry, find_model_by_table, models};
pub use relations::{
    AggregateKind, BelongsTo, BelongsToMany, EagerLoadCache, EagerLoadDispatch, HasMany,
    HasManyThrough, HasOne, HasOneThrough, MorphMany, MorphOne, MorphTo, MorphToMany,
    MorphTypeEntry, MorphedByMany, Relation, RelationEntry, RelationKind, aggregate_cache_key,
    find_morph_type, find_morph_type_by_id, find_relation, morph_types, relations, relations_of,
};
pub use scopes::{GlobalScope, ScopeRegistry};
pub use soft_deletes::SoftDeletes;
pub use timestamps::{Touchable, touches_disabled, without_touching};
pub use unique_id::{HasUniqueId, UniqueIdKind};

/// Marker trait emitted by `#[suprnova::model]`. Indicates the struct
/// is a Suprnova-managed model.
///
/// This trait grows across Phase 10A tasks (T3 / T4 / T6 / T7a / ...);
/// the stable shape locks at T11 closeout.
pub trait EloquentModel: Sized {
    type Entity: crate::EntityTrait;
    type Column;
    const TABLE: &'static str;
    /// Primary-key column name. The macro emits the value from the
    /// `primary_key = "..."` attribute (default `"id"`). Mirrors
    /// [`crate::eloquent::Model::primary_key_name`] but as a `const`
    /// so it can be read by `inventory::submit!` initialisers — the
    /// has/where-has engine pulls each relation's target PK from here
    /// at link time to render the correct pivot join.
    const PRIMARY_KEY: &'static str = "id";
    /// Soft-delete column on this model. `""` when the model does NOT
    /// opt into `#[model(soft_deletes)]`; otherwise the model's
    /// configured `deleted_at` column name. Read by the has/where-has
    /// engine to auto-apply the related model's soft-delete scope to
    /// EXISTS subqueries (a parent with only soft-deleted children
    /// must NOT match `has("children")`).
    const SOFT_DELETES_COLUMN: &'static str = "";
}
