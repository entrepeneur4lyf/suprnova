//! `Persistable` trait — the seam between a factory-produced model and
//! whatever storage it lives in.
//!
//! The trait surface is intentionally minimal — `async fn persist(self)
//! -> Result<Self, FrameworkError>` — so a custom backend (Redis,
//! Surreal, blob-only, whatever) can opt in with one impl.
//!
//! ## SeaORM blanket impl
//!
//! Every SeaORM `Model` that can `IntoActiveModel<ActiveModel>` already
//! gets `Persistable` via the blanket impl below. `persist(self)` pulls
//! the framework's bound `DB::connection()`, converts to an
//! `ActiveModel`, and `.insert(...)`s. The returned `Self` is what
//! SeaORM hands back from the insert — with the auto-incremented id,
//! default-filled columns, etc. resolved.
//!
//! No per-model boilerplate. `User::factory().count(50).create_many()`
//! works as soon as `User` is a SeaORM entity.
//!
//! ## Sharp edge — orphan rules
//!
//! Because the blanket targets every `ModelTrait` type, a downstream
//! crate cannot write its own `impl Persistable for MyOrm::Model`
//! without `MyOrm::Model: ModelTrait` (which would conflict). For
//! non-SeaORM custom-persist scenarios, wrap the model in a newtype
//! and impl `Persistable` on the wrapper. This is a deliberate
//! trade-off — SeaORM is the first-class ORM and the ergonomic win on
//! the dogfood path is worth the orphan constraint.
//!
//! ## Helper for explicit-connection sites
//!
//! [`persist_via_seaorm`] takes the connection as an argument for the
//! rare case where a caller wants to drive persistence against a
//! connection that ISN'T the framework's bound `DB::connection()` —
//! integration tests that want to verify against a specific
//! `sqlite::memory:` handle, for instance.

use crate::error::FrameworkError;
use async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, EntityTrait, IntoActiveModel, Iterable, ModelTrait,
    PrimaryKeyToColumn,
};

/// A model that can persist itself. The async method consumes `self`
/// and returns the canonicalized post-insert version (assigned id,
/// defaulted columns resolved, etc.).
#[async_trait]
pub trait Persistable: Sized + Send {
    async fn persist(self) -> Result<Self, FrameworkError>;
}

/// SeaORM-backed persist against a specific connection. Useful when a
/// caller wants to drive persistence against a connection that isn't
/// the framework's bound `DB::connection()` — most often an
/// `sqlite::memory:` handle in an integration test.
///
/// `M` is the SeaORM Model; `E` is its Entity. The trait bounds thread
/// through SeaORM's type system to require:
///   - `M: ModelTrait<Entity = E>` — M IS the canonical model of E
///   - `M: IntoActiveModel<E::ActiveModel>` — M can become an active model
///   - `E::ActiveModel: ActiveModelTrait<Entity = E>` — round-trip closes
///
/// # Primary-key handling
///
/// `IntoActiveModel`'s derive sets EVERY field — including the primary
/// key — to `Set(value)`. For factory-produced models the PK is
/// typically a placeholder (`0` for an auto-increment `i32`), so a
/// straight insert collides on the second call (UNIQUE constraint
/// failure). This helper flips every primary-key column to
/// `NotSet` BEFORE inserting, which lets the database assign its own
/// id — the exact semantic factories need.
///
/// Consumers who DO want to assign a specific id can ignore this
/// helper and call `model.into_active_model().insert(db)` directly;
/// the seam is there for the auto-increment dogfood path.
pub async fn persist_via_seaorm<M, E, C>(model: M, db: &C) -> Result<M, FrameworkError>
where
    M: ModelTrait<Entity = E> + IntoActiveModel<<E as EntityTrait>::ActiveModel> + Send,
    E: EntityTrait<Model = M>,
    <E as EntityTrait>::ActiveModel: ActiveModelTrait<Entity = E> + Send,
    <E as EntityTrait>::PrimaryKey:
        PrimaryKeyToColumn<Column = <E as EntityTrait>::Column> + Iterable,
    C: ConnectionTrait,
{
    let mut active = model.into_active_model();
    // Flip every primary-key column to NotSet so the DB assigns it.
    // Composite PKs walk every column in the iter.
    for pk in <<E as EntityTrait>::PrimaryKey as Iterable>::iter() {
        active.not_set(pk.into_column());
    }
    active
        .insert(db)
        .await
        .map_err(|e| FrameworkError::internal(format!("factory persist: {e}")))
}

/// Blanket impl: any SeaORM `Model` is `Persistable` via
/// `DB::connection()`. Consumers don't write per-model
/// `impl Persistable for User` — the trait is already there.
#[async_trait]
impl<M, E> Persistable for M
where
    M: ModelTrait<Entity = E> + IntoActiveModel<<E as EntityTrait>::ActiveModel> + Send + 'static,
    E: EntityTrait<Model = M>,
    <E as EntityTrait>::ActiveModel: ActiveModelTrait<Entity = E> + Send,
    <E as EntityTrait>::PrimaryKey:
        PrimaryKeyToColumn<Column = <E as EntityTrait>::Column> + Iterable,
{
    async fn persist(self) -> Result<Self, FrameworkError> {
        let db = crate::database::DB::connection()?;
        persist_via_seaorm(self, db.inner()).await
    }
}
