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
pub mod fillable;
pub mod model;
pub mod registry;

/// Runtime cast bridge. T7a fills in the full module; T5 ships the
/// trait stub so [`Builder::with_casts`] can compile against
/// `Arc<dyn DynCast>` in this module's namespace. The `Send + Sync`
/// supertraits keep the trait-object usable across `tokio` task
/// boundaries.
pub mod casts {
    use crate::error::FrameworkError;

    /// Storage-shape ↔ rust-shape cast applied at row materialisation
    /// (`from_storage_json`) and at write (`to_storage_json`). T5
    /// ships the surface only; T7a wires the per-cast implementations
    /// (Json, Encrypted, Decimal, ...).
    ///
    /// The `from_*` / `to_*` names take `&self` because the cast
    /// instance carries config (e.g. the encryption key for an
    /// `Encrypted` cast); they're not Rust's conventional consume-self
    /// constructors. Clippy's `wrong_self_convention` lint is allowed
    /// here for that reason.
    #[allow(clippy::wrong_self_convention)]
    pub trait DynCast: Send + Sync {
        /// Convert a raw storage value into the in-memory shape (e.g.
        /// decode a JSON column into a `serde_json::Value`).
        fn from_storage_json(
            &self,
            v: &serde_json::Value,
        ) -> Result<serde_json::Value, FrameworkError>;

        /// Convert an in-memory value into its storage shape (e.g.
        /// encode a `serde_json::Value` back into a JSON string for
        /// the underlying TEXT column).
        fn to_storage_json(
            &self,
            v: &serde_json::Value,
        ) -> Result<serde_json::Value, FrameworkError>;
    }
}

pub use attrs::Attrs;
pub use builder::{Builder, Direction, IntoColumn, IntoVal};
pub use fillable::{unguarded, Fillable};
pub use model::{FirstOrCreate, Model, ReplicateExt};
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
