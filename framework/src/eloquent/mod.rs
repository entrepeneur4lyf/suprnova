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

pub mod builder;
pub mod registry;

pub use registry::{find_model_by_table, models, ModelEntry};

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
