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
pub mod fillable;
pub mod model;
pub mod prunable;
pub mod registry;
pub mod relations;
pub mod soft_deletes;
pub mod timestamps;

pub use attrs::Attrs;
pub use builder::{Builder, Direction, IntoColumn, IntoVal};
pub use casts::{
    AsArray, AsArrayObject, AsBool, AsCollection, AsDate, AsDateTime, AsDecimal, AsEncrypted,
    AsEncryptedArray, AsEncryptedCollection, AsEncryptedObject, AsEnum, AsFloat, AsHashed,
    AsImmutableDate, AsImmutableDateTime, AsInt, AsJson, AsObject, AsOptionalDateTime, AsString,
    AsTimestamp, Cast, DynCast, IntoDynCast,
};
pub use collection::Collection;
pub use fillable::{unguarded, Fillable};
pub use model::{FirstOrCreate, Model, ReplicateExt};
pub use prunable::{
    prune_all, prune_all_dry, prune_one, pruners, MassPrunable, Prunable, PrunerEntry, PrunerFn,
};
pub use registry::{find_model_by_table, models, ModelEntry};
pub use relations::{
    find_relation, relations, relations_of, AggregateKind, BelongsTo, EagerLoadCache,
    EagerLoadDispatch, HasMany, HasOne, Relation, RelationEntry, RelationKind,
};
pub use soft_deletes::SoftDeletes;
pub use timestamps::Touchable;

/// Marker trait emitted by `#[suprnova::model]`. Indicates the struct
/// is a Suprnova-managed model.
///
/// This trait grows across Phase 10A tasks (T3 / T4 / T6 / T7a / ...);
/// the stable shape locks at T11 closeout.
pub trait EloquentModel: Sized {
    type Entity: crate::EntityTrait;
    type Column;
    const TABLE: &'static str;
}
