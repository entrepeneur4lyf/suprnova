//! `HasOne` — one-to-one from parent to child.
//!
//! Mirrors Laravel's [`hasOne`](https://laravel.com/docs/12.x/eloquent-relationships#one-to-one)
//! semantics: the child table carries a foreign key pointing at the
//! parent. Default FK convention: `<parent_snake>_id`. Default LK
//! (the parent column the FK matches against): `"id"`.
//!
//! Chainable — `user.profile().filter("verified", true).first().await?`
//! flows through the inner [`Builder<R>`]. The dual-API surface
//! (`filter` / `db_where`) is honoured on the wrapper too: both names
//! forward to the inner builder so callers writing Laravel-flavoured
//! code aren't forced to switch dialects mid-chain.
//!
//! The macro emits a `<rel>(&self) -> HasOne<Self, Target>` accessor
//! per `relations = { rel: HasOne<Target> }` declaration; user code
//! never invokes [`HasOne::__new`] directly. Customisation flows
//! through the `fk = "..."` / `lk = "..."` inline options on the
//! relation declaration — the macro bakes the chosen keys into the
//! call to [`HasOne::__new`].
//!
//! Eager-load orchestration lives in the parent model's
//! `__eager_load("<rel>", ...)` match arm; this struct itself only
//! handles the lazy `.first()` / `.get()` path.

use std::marker::PhantomData;

use crate::eloquent::EloquentModel;
use crate::eloquent::builder::{Builder, IntoColumn, IntoVal};
use crate::eloquent::collection::Collection;
use crate::eloquent::model::Model;
use crate::eloquent::relations::{Relation, RelationKind};
use crate::error::FrameworkError;

/// One-to-one relation from parent `L` to child `R`. Constructed by
/// the macro-emitted relation method (`fn profile(&self) -> HasOne<Self, Profile>`);
/// user code never calls [`HasOne::__new`] directly.
///
/// The wrapper carries the FK / LK metadata plus a pre-filtered
/// `Builder<R>` that already has `WHERE <fk> = <parent_key_value>`
/// applied. Chaining `filter` / `db_where` forwards onto that builder
/// so the relation method composes with the rest of the dual-API.
pub struct HasOne<L, R>
where
    L: EloquentModel,
    R: Model,
    R: From<<R::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <R::Entity as sea_orm::EntityTrait>::Model: From<R>
        + sea_orm::IntoActiveModel<<R::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <R::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<R::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Parent row's local-key value (typically `parent.id`) — the
    /// right-hand side of the inner builder's `WHERE fk = ?` predicate.
    /// Stored as `serde_json::Value` because the builder's `WhereTerm`
    /// holds JSON; the macro emits `serde_json::to_value(&self.id)`
    /// at the call site so the conversion is paid once at construction
    /// (and the framework can serialise any FK type the user might
    /// declare — `i64`, `String`, `Uuid`-via-string, etc.).
    parent_key_value: serde_json::Value,
    /// Column on the child table that points at the parent.
    foreign_key: String,
    /// Column on the parent table the FK matches against. Stored for
    /// the [`Relation`] impl; the inner builder doesn't need it
    /// directly (the FK constraint runs against the parent row's
    /// already-extracted value).
    parent_key: String,
    /// Pre-filtered builder against the child table.
    inner: Builder<R>,
    /// PhantomData carries the parent type so the [`Relation`] impl
    /// can name `type Parent = L` without a runtime field. `fn() -> L`
    /// keeps the type covariant + `Send + Sync` regardless of `L`.
    _phantom: PhantomData<fn() -> L>,
}

// The `M: Model` bound on the impl block re-elaborates Model's
// supertrait where-clauses for the same reason `Builder<M: Model>` in
// `builder.rs` does: Rust's trait elaboration doesn't transitively
// propagate associated-type bounds from a supertrait's where clause to
// methods on a downstream `impl`. Without them, calling
// `R::query().filter(...)` inside `__new` fails because `Builder<R>`
// can't prove its own bounds are satisfied for an arbitrary `R: Model`.
impl<L, R> HasOne<L, R>
where
    L: EloquentModel,
    R: Model,
    R: From<<R::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <R::Entity as sea_orm::EntityTrait>::Model: From<R>
        + sea_orm::IntoActiveModel<<R::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <R::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<R::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Construct a `HasOne` from the parent row's key value + the FK
    /// and LK column names. Invoked by the macro-emitted relation
    /// method; not part of the public API.
    ///
    /// `parent_key_value` is the JSON-serialised parent PK (e.g.
    /// `serde_json::to_value(&self.id)`) — the macro pays for the
    /// conversion once at construction so the builder's JSON-shaped
    /// `WhereTerm` storage stays homogeneous.
    #[doc(hidden)]
    pub fn __new(
        parent_key_value: serde_json::Value,
        foreign_key: String,
        parent_key: String,
    ) -> Self {
        let inner = R::query().filter(foreign_key.as_str(), parent_key_value.clone());
        Self {
            parent_key_value,
            foreign_key,
            parent_key,
            inner,
            _phantom: PhantomData,
        }
    }

    /// Override the FK column post-construction. Rare in practice
    /// (the macro's `fk = "..."` option covers the static case); kept
    /// for parity with Laravel's chainable `->foreignKey(...)`.
    ///
    /// Rebuilds the inner builder from scratch so the new FK column
    /// replaces (not augments) the original `WHERE fk = ?` predicate.
    pub fn foreign_key(mut self, key: impl Into<String>) -> Self {
        self.foreign_key = key.into();
        self.inner = R::query().filter(self.foreign_key.as_str(), self.parent_key_value.clone());
        self
    }

    /// Override the LK column post-construction. Only updates the
    /// metadata the [`Relation`] impl exposes — the inner builder
    /// already holds the LK's *value* (extracted from the parent row
    /// at construction time), so the column name only matters for
    /// eager-load dispatchers reading [`Relation::parent_key`].
    pub fn local_key(mut self, key: impl Into<String>) -> Self {
        self.parent_key = key.into();
        self
    }

    /// Chainable `WHERE col = val` on the inner builder. Same shape as
    /// `Builder::filter`; the dual-API alias [`Self::db_where`]
    /// forwards here.
    pub fn filter(mut self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.inner = self.inner.filter(col, val);
        self
    }

    /// Laravel-shape alias for [`Self::filter`].
    pub fn db_where(self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.filter(col, val)
    }

    /// Execute the inner builder and return the first matching row.
    /// Returns `None` when the child table has no row pointing at
    /// this parent.
    pub async fn first(self) -> Result<Option<R>, FrameworkError> {
        self.inner.first().await
    }

    /// Execute the inner builder and return every matching row.
    /// Most callers want [`Self::first`]; `get()` is here for users
    /// who want the unfiltered set without dropping into
    /// `Self::query().filter(...).get()` themselves.
    ///
    /// Returns a [`Collection<R>`](crate::eloquent::Collection); see
    /// [`HasMany::get`](super::HasMany::get) for return-type
    /// rationale.
    pub async fn get(self) -> Result<Collection<R>, FrameworkError> {
        self.inner.get().await
    }
}

/// Soft-delete scope modifiers for relations whose **target** model
/// is soft-deletable. The forwarding methods route through the inner
/// `Builder<R>`'s `with_trashed` / `only_trashed`, which themselves
/// only exist when `R: SoftDeletes` — so this impl is the only place
/// the soft-delete escape hatch is reachable from a `HasOne` chain.
impl<L, R> HasOne<L, R>
where
    L: EloquentModel,
    R: Model + crate::eloquent::SoftDeletes,
    R: From<<R::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <R::Entity as sea_orm::EntityTrait>::Model: From<R>
        + sea_orm::IntoActiveModel<<R::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <R::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<R::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Widen the relation to include trashed children. Mirrors
    /// `Builder<R>::with_trashed` on the inner builder.
    pub fn with_trashed(mut self) -> Self {
        self.inner = self.inner.with_trashed();
        self
    }

    /// Restrict the relation to *only* trashed children. Mirrors
    /// `Builder<R>::only_trashed` on the inner builder.
    pub fn only_trashed(mut self) -> Self {
        self.inner = self.inner.only_trashed();
        self
    }
}

impl<L, R> Relation for HasOne<L, R>
where
    L: EloquentModel,
    R: Model,
    R: From<<R::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <R::Entity as sea_orm::EntityTrait>::Model: From<R>
        + sea_orm::IntoActiveModel<<R::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <R::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<R::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    type Parent = L;
    type Target = R;
    const KIND: RelationKind = RelationKind::HasOne;

    fn parent_key(&self) -> &str {
        &self.parent_key
    }

    fn foreign_key(&self) -> &str {
        &self.foreign_key
    }
}
