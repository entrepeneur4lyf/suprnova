//! Relations — Laravel-shape one-to-one / one-to-many / many-to-many
//! and polymorphic relations layered over SeaORM JOINs and `IN` queries.
//!
//! Phase 10B foundation (T1). Each concrete relation type (`HasOne`,
//! `BelongsTo`, `HasMany`, `BelongsToMany`, `HasOneThrough`,
//! `HasManyThrough`, `MorphTo`, `MorphOne`, `MorphMany`, `MorphToMany`,
//! `MorphedByMany`) implements [`Relation`] and is dispatched from a
//! per-model `__eager_load` match arm. The
//! `#[suprnova::model(relations = { ... })]` macro emits, per declared
//! relation:
//!
//! 1. A relation method (`fn posts(&self) -> HasMany<Self, Post>`) —
//!    bodies land in T2-T7 (this task ships placeholder skeletons only
//!    if the kind is supported).
//! 2. A loaded-accessor (`posts_loaded() -> &[Post]`).
//! 3. A count-accessor (`posts_count() -> u64`).
//! 4. A `match` arm in the model's `__eager_load` dispatcher (skeleton
//!    here; arms land in T2-T7).
//! 5. An `inventory::submit!(RelationEntry { ... })` for Phase 8
//!    enumeration.
//!
//! T1 ships:
//! - The [`Relation`] sealed trait.
//! - The [`RelationKind`] enum enumerating every flavour up-front.
//! - The [`AggregateKind`] enum for `with_sum` / `with_avg` /
//!   `with_min` / `with_max`.
//! - The [`RelationEntry`] inventory type + helpers
//!   ([`relations`], [`relations_of`], [`find_relation`]).
//! - The [`EagerLoadCache`] storage type (in
//!   [`eager_cache`][crate::eloquent::relations::eager_cache]).
//! - The macro-emitted `__eager` / `__pivot` field auto-injection, the
//!   four dispatcher skeletons (`__eager_load`, `__recurse_eager_load`,
//!   `__count_relation`, `__aggregate_relation`), and the
//!   `pivot::<P>()` accessor. Those live on the user struct via
//!   `#[suprnova::model]` and are exercised by the integration tests.

pub mod belongs_to;
pub mod belongs_to_many;
pub(crate) mod eager;
pub mod eager_cache;
pub mod has_many;
pub mod has_one;
pub mod morph;
pub mod morph_registry;
pub mod morph_to_many;
pub mod through;

pub use belongs_to::BelongsTo;
pub use belongs_to_many::BelongsToMany;
pub use eager_cache::EagerLoadCache;
pub use has_many::HasMany;
pub use has_one::HasOne;
pub use morph::{MorphMany, MorphOne, MorphTo};
pub use morph_registry::{find_morph_type, find_morph_type_by_id, morph_types, MorphTypeEntry};
pub use morph_to_many::{MorphToMany, MorphedByMany};
pub use through::{HasManyThrough, HasOneThrough};

use std::any::{Any, TypeId};
use std::future::Future;
use std::pin::Pin;

use sea_orm::DatabaseConnection;

use crate::error::FrameworkError;

/// The exhaustive list of Eloquent relation flavours Suprnova ships.
///
/// New flavours can NOT be added by user code — the macro pattern-
/// matches on this enum exhaustively. v2 ask for plugin-loader-
/// registered relation types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    HasOne,
    BelongsTo,
    HasMany,
    BelongsToMany,
    HasOneThrough,
    HasManyThrough,
    MorphTo,
    MorphOne,
    MorphMany,
    MorphToMany,
    MorphedByMany,
}

/// Aggregate flavour for `with_sum` / `with_avg` / `with_min` /
/// `with_max`. Passed into the per-model `__aggregate_relation`
/// dispatcher so a single dispatcher per model covers all four
/// aggregates without exploding into per-kind methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    Sum,
    Avg,
    Min,
    Max,
}

impl AggregateKind {
    /// Lower-case spelling used inside aggregate cache keys
    /// (`"sum"` / `"avg"` / `"min"` / `"max"`). Stable wire-style
    /// representation — do not change without bumping the cache-key
    /// contract.
    pub fn as_key_str(self) -> &'static str {
        match self {
            AggregateKind::Sum => "sum",
            AggregateKind::Avg => "avg",
            AggregateKind::Min => "min",
            AggregateKind::Max => "max",
        }
    }
}

/// Build the wide cache key the aggregate dispatcher arms write into
/// `EagerLoadCache::set_aggregate`. The shape is `<rel>_<kind>_<col>`
/// — `with_sum(("posts","id"))` lands under `"posts_sum_id"`,
/// `with_avg(("posts","id"))` under `"posts_avg_id"`, etc. — so a
/// single eager-load plan can stack multiple aggregates on the same
/// relation without colliding on the cache cell.
///
/// Count keys keep the unadorned `<rel>` form (separate
/// `RelationCell::Count(u64)` variant; zero collision risk with the
/// aggregate cell).
///
/// This helper is the single source of truth for the key format. The
/// macro's aggregate arms call it on write; the per-relation
/// `<rel>_sum_of(col)` / `_avg_of` / `_min_of` / `_max_of` accessors
/// it emits call it on read. Don't hand-format the key elsewhere.
pub fn aggregate_cache_key(name: &str, kind: AggregateKind, column: &str) -> String {
    let mut s = String::with_capacity(name.len() + 5 + column.len());
    s.push_str(name);
    s.push('_');
    s.push_str(kind.as_key_str());
    s.push('_');
    s.push_str(column);
    s
}

/// Sealed trait every concrete relation type implements.
///
/// "Sealed" in the sense that all impl sites live inside the framework
/// crate — user code never hand-writes a `Relation` impl. The macro
/// emits all impls from `#[model(relations = { ... })]` declarations.
///
/// The trait carries the metadata an eager loader needs without
/// knowing the concrete relation type — `KIND` for dispatch,
/// `parent_key` + `foreign_key` for the `IN` query, and the associated
/// `Parent` / `Target` types for compile-time wiring.
pub trait Relation {
    /// The owning model (the side that calls `self.has_many::<R>()`).
    type Parent;
    /// The related model.
    type Target;
    /// Compile-time relation kind. Drives the dispatcher's branch.
    const KIND: RelationKind;
    /// Column name on the parent table used as the join key.
    /// Defaults to `"id"` in concrete impls; customisable per-relation
    /// via the macro's `lk = "..."` option.
    fn parent_key(&self) -> &str;
    /// Column name on the target table that points at the parent.
    ///
    /// For `BelongsTo`: column on the CHILD that points at the PARENT.
    /// For polymorphic relations this is the `*_id` column (with a
    /// sibling `*_type` discriminator handled inside the dispatcher).
    fn foreign_key(&self) -> &str;
}

/// Compile-time entry per relation. Submitted via `inventory::submit!`
/// by the `#[suprnova::model]` macro for every relation declared in
/// `relations = { ... }`.
///
/// Phase 8 (Admin) walks this registry to enumerate every relation in
/// the binary; the eager loader does NOT use it (each model has a
/// typed per-relation match arm in its `__eager_load` dispatcher
/// instead). The type-erased `fn() -> TypeId` shape keeps the entry
/// `Copy` so `inventory::submit!` accepts it as a const initialiser.
#[derive(Debug, Clone, Copy)]
pub struct RelationEntry {
    /// `TypeId::of::<L>` — the owning model.
    pub parent_type: fn() -> TypeId,
    /// `TypeId::of::<R>` — the related model. For `MorphTo` this is
    /// `TypeId::of::<()>` because the target is a per-family enum
    /// generated by T6, not a single concrete type.
    pub target_type: fn() -> TypeId,
    /// Relation name as declared (`"posts"`, `"commentable"`, ...).
    pub name: &'static str,
    /// Relation kind.
    pub kind: RelationKind,
    /// Owning model's type name (`"User"`).
    pub parent_type_name: &'static str,
    /// Related model's type name (`"Post"`). For `MorphTo` this is
    /// `"<morph>"` — the per-family enum type name lives in the
    /// generated code, not in the entry.
    pub target_type_name: &'static str,
}

inventory::collect!(RelationEntry);

/// Iterator over every relation declared anywhere in the binary.
///
/// Order is link-time; do not depend on it.
pub fn relations() -> impl Iterator<Item = &'static RelationEntry> {
    inventory::iter::<RelationEntry>()
}

/// Find every relation declared on a specific parent type.
pub fn relations_of<T: 'static>() -> impl Iterator<Item = &'static RelationEntry> {
    let want = TypeId::of::<T>();
    relations().filter(move |e| (e.parent_type)() == want)
}

/// Find one relation by parent type + relation name. Returns `None`
/// if the model has no relation by that name registered.
pub fn find_relation<T: 'static>(name: &str) -> Option<&'static RelationEntry> {
    let want = TypeId::of::<T>();
    relations().find(|e| (e.parent_type)() == want && e.name == name)
}

// ---- T2: EagerLoadDispatch trait ----------------------------------------
//
// `Builder<M>::with([...])` records relation names; `Builder<M>::get`
// must call `M::__eager_load(name, &mut [&mut row, ...], db, predicate)`
// for each one. The four dispatcher methods land on the user struct
// as inherent methods (emitted by `#[suprnova::model]`); a generic
// `Builder<M>` can't reach them without a trait. T2 introduces this
// sealed trait so the macro can emit a delegating impl per model.
//
// The trait carries one method per dispatcher kind (`eager_load`,
// `count_relation`, `aggregate_relation`, `recurse_eager_load`); T2
// only uses `eager_load` from `Builder::get`. T3-T7 keep adding
// per-kind match arms inside the inherent dispatcher methods — those
// changes never touch the trait surface, since the trait is just a
// thin pass-through.

/// Language-level seal for [`EagerLoadDispatch`].
///
/// The module is `pub` but doc-hidden — the macro-emitted impl in the
/// user's crate needs a public path to reach [`Sealed`][__sealed::Sealed],
/// but downstream code that finds it has gone out of its way to reach
/// past the framework convention reserving leading-double-underscore
/// names (`__eager`, `__pivot`, `__async_trait`) for framework-private
/// machinery. The trait is empty: hand-rolling an `impl Sealed for X`
/// alone doesn't get you a working `EagerLoadDispatch` — you'd also
/// need to hand-roll every dispatcher method on `X`, which is exactly
/// what `#[suprnova::model]` exists to emit.
///
/// To manually verify the seal blocks user impls of
/// `EagerLoadDispatch`, attempt to implement the trait without the
/// `Sealed` bound being satisfied:
///
/// ```compile_fail
/// use std::any::Any;
/// use std::future::Future;
/// use std::pin::Pin;
/// use suprnova::eloquent::{AggregateKind, EagerLoadDispatch};
/// use suprnova::sea_orm::DatabaseConnection;
/// use suprnova::FrameworkError;
///
/// struct NotAModel;
///
/// impl EagerLoadDispatch for NotAModel {
///     fn eager_load<'a>(
///         _r: &'a str,
///         _p: &'a mut [&'a mut Self],
///         _d: &'a DatabaseConnection,
///         _x: Option<Box<dyn Any + Send + Sync>>,
///     ) -> Pin<Box<dyn Future<Output = Result<(), FrameworkError>> + Send + 'a>> {
///         unimplemented!()
///     }
///     fn count_relation<'a>(
///         _r: &'a str,
///         _p: &'a mut [&'a mut Self],
///         _d: &'a DatabaseConnection,
///     ) -> Pin<Box<dyn Future<Output = Result<(), FrameworkError>> + Send + 'a>> {
///         unimplemented!()
///     }
///     fn aggregate_relation<'a>(
///         _r: &'a str,
///         _c: &'a str,
///         _k: AggregateKind,
///         _p: &'a mut [&'a mut Self],
///         _d: &'a DatabaseConnection,
///     ) -> Pin<Box<dyn Future<Output = Result<(), FrameworkError>> + Send + 'a>> {
///         unimplemented!()
///     }
///     fn recurse_eager_load<'a>(
///         &'a mut self,
///         _r: &'a str,
///         _rs: &'a str,
///         _d: &'a DatabaseConnection,
///         _missing_only: bool,
///     ) -> Pin<Box<dyn Future<Output = Result<(), FrameworkError>> + Send + 'a>> {
///         unimplemented!()
///     }
///     fn set_pivot_arc(
///         &mut self,
///         _p: Option<std::sync::Arc<dyn Any + Send + Sync>>,
///     ) {
///         unimplemented!()
///     }
/// }
/// ```
///
/// The compiler rejects this with: *the trait bound `NotAModel:
/// __sealed::Sealed` is not satisfied*.
#[doc(hidden)]
pub mod __sealed {
    /// Sealed marker — only the `#[suprnova::model]` macro implements
    /// this for user structs.
    pub trait Sealed {}
}

/// Bridge from the eager-load orchestrator (`Builder<M>::get`) to the
/// macro-emitted per-model `__eager_load` / `__count_relation` /
/// `__aggregate_relation` / `__recurse_eager_load` inherent methods.
///
/// **Sealed.** Implemented automatically by `#[suprnova::model]`; user
/// code cannot hand-write an impl — the [`__sealed::Sealed`]
/// supertrait blocks it. Returning `Pin<Box<dyn Future>>` (rather than
/// `async fn`) keeps the trait object-safety friendly — `async fn`
/// trait methods would force `Builder<M>` to carry a Pin<Box<...>>
/// state itself, complicating the type. For T2 we don't actually need
/// `dyn EagerLoadDispatch`, but the boxed-future shape stays cleanest
/// across the bound site.
pub trait EagerLoadDispatch: __sealed::Sealed + Sized {
    /// Delegate to the per-model `__eager_load` dispatcher.
    fn eager_load<'a>(
        relation: &'a str,
        parents: &'a mut [&'a mut Self],
        db: &'a DatabaseConnection,
        predicate: Option<Box<dyn Any + Send + Sync>>,
    ) -> Pin<Box<dyn Future<Output = Result<(), FrameworkError>> + Send + 'a>>;

    /// Delegate to `__count_relation`.
    fn count_relation<'a>(
        relation: &'a str,
        parents: &'a mut [&'a mut Self],
        db: &'a DatabaseConnection,
    ) -> Pin<Box<dyn Future<Output = Result<(), FrameworkError>> + Send + 'a>>;

    /// Delegate to `__aggregate_relation`.
    fn aggregate_relation<'a>(
        relation: &'a str,
        column: &'a str,
        kind: AggregateKind,
        parents: &'a mut [&'a mut Self],
        db: &'a DatabaseConnection,
    ) -> Pin<Box<dyn Future<Output = Result<(), FrameworkError>> + Send + 'a>>;

    /// Delegate to `__recurse_eager_load`. Used by T9's nested-path
    /// resolver; T2 doesn't call this from `Builder::get`.
    ///
    /// `missing_only` switches per-relation arm behaviour: when
    /// `false` (the default, used by [`Builder::with`]) the arm
    /// unconditionally bulk-loads the next segment on the cached
    /// children. When `true` (used by
    /// [`Collection::load_missing`][crate::eloquent::Collection::load_missing])
    /// the arm first checks whether any cached child already has the
    /// next segment loaded, and skips the bulk-load if so. The flag
    /// propagates through the tail recursion so a dotted path like
    /// `"posts.comments.author"` keeps the "skip already cached" rule
    /// at every level.
    fn recurse_eager_load<'a>(
        &'a mut self,
        relation: &'a str,
        rest: &'a str,
        db: &'a DatabaseConnection,
        missing_only: bool,
    ) -> Pin<Box<dyn Future<Output = Result<(), FrameworkError>> + Send + 'a>>;

    /// Stamp the per-row `__pivot` field with a type-erased pivot row.
    /// Used by [`BelongsToMany::get`](super::belongs_to_many::BelongsToMany::get)
    /// to attach pivot context to each related row at load time, when
    /// the related type is generic and can't reach `self.__pivot`
    /// directly through field access.
    ///
    /// Implemented automatically by `#[suprnova::model]` as
    /// `self.__pivot = pivot;`. Not part of the user surface; the
    /// macro-emitted `pivot::<P>()` accessor is the read path users
    /// call.
    fn set_pivot_arc(
        &mut self,
        pivot: Option<std::sync::Arc<dyn Any + Send + Sync>>,
    );

    /// Whether the per-row `__eager` cache has a value for the named
    /// relation. Used by
    /// [`Collection<M>::load_missing`][crate::eloquent::Collection::load_missing]
    /// to skip already-loaded relations.
    ///
    /// Implemented automatically by `#[suprnova::model]` as
    /// `self.__eager.has(name)`. Not part of the user surface — the
    /// `<rel>_loaded()` accessor is the user-side read path.
    fn has_eager(&self, name: &str) -> bool;
}

#[cfg(test)]
mod seal_tests {
    //! Sanity-check the [`__sealed::Sealed`] trait is reachable from
    //! the documented path. The strong negative ("user code cannot
    //! impl `EagerLoadDispatch`") is pinned by the `compile_fail`
    //! doctest on [`__sealed`]; this test only confirms the seal
    //! module is wired and the supertrait bound holds.

    use super::__sealed::Sealed;

    /// A framework-side type that opts into the seal — the test
    /// passes if this compiles, confirming `Sealed` is reachable and
    /// implementable inside the framework crate. The type itself is
    /// never constructed; its `impl Sealed` is the assertion.
    #[allow(dead_code)]
    struct InCrate;
    impl Sealed for InCrate {}

    /// Compile-time check: any `T: EagerLoadDispatch` is also
    /// `T: Sealed`. If the supertrait gets accidentally dropped from
    /// `EagerLoadDispatch`, this stops compiling.
    fn _supertrait_bound_holds<T: super::EagerLoadDispatch>(_: &T) {
        fn requires_sealed<S: Sealed>() {}
        requires_sealed::<T>();
    }
}
