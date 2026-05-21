//! `HasMany` — one-to-many from parent to children.
//!
//! Mirrors Laravel's
//! [`hasMany`](https://laravel.com/docs/12.x/eloquent-relationships#one-to-many)
//! semantics: the child table carries a foreign key pointing at the
//! parent. Default FK convention: `<parent_snake>_id`. Default LK
//! (the parent column the FK matches against): `"id"`. Both
//! customisable through the macro's `fk = "..."` / `lk = "..."`
//! options.
//!
//! Chainable — `user.posts().latest().take(5).get().await?` flows
//! through the inner [`Builder<R>`]. The dual-API surface
//! (`filter` / `db_where`) is honoured on the wrapper too, matching
//! the [`HasOne`](super::HasOne) shape so callers writing Laravel-
//! flavoured code don't have to switch dialects mid-chain.
//!
//! The chainable surface covers `filter` / `db_where` / `order_by` /
//! `latest` / `oldest` / `limit` / `take` and the terminal
//! `first` / `get` / `count`. `latest()` / `oldest()` are sugar for
//! `order_by("created_at", ...)` — they only resolve when the
//! related model declares a `created_at` column (which the
//! `#[suprnova::model]` macro auto-adds when timestamps are on).
//!
//! Customisation flows through the `fk = "..."` / `lk = "..."` inline
//! options on the relation declaration — the macro bakes the chosen
//! keys into the call to [`HasMany::__new`].
//!
//! Eager-load orchestration lives in the parent model's
//! `__eager_load("<rel>", ...)` match arm; this struct itself only
//! handles the lazy `.first()` / `.get()` / `.count()` path.

use std::marker::PhantomData;

use crate::eloquent::builder::{Builder, Direction, IntoColumn, IntoVal};
use crate::eloquent::collection::Collection;
use crate::eloquent::model::Model;
use crate::eloquent::relations::{Relation, RelationKind};
use crate::eloquent::EloquentModel;
use crate::error::FrameworkError;

/// One-to-many relation from parent `L` to children `R`. Constructed
/// by the macro-emitted relation method (`fn posts(&self) -> HasMany<Self, Post>`);
/// user code never calls [`HasMany::__new`] directly.
///
/// The wrapper carries the FK / LK metadata plus a pre-filtered
/// [`Builder<R>`] with `WHERE <fk> = <parent_key_value>` already
/// applied. Chaining `filter` / `order_by` / `limit` forwards onto
/// that builder so the relation method composes cleanly with the rest
/// of the dual-API.
pub struct HasMany<L, R>
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
    /// Parent row's local-key value, JSON-encoded. Stored as
    /// `serde_json::Value` for the same reason
    /// [`HasOne`](super::HasOne) does — the builder's `WhereTerm`
    /// storage holds JSON, and converting once at construction keeps
    /// the FK-typing flexibility (i64 / String / Uuid-via-string)
    /// without polluting every chainable call with a `T: IntoVal`
    /// bound.
    parent_key_value: serde_json::Value,
    /// Column on the child table that points at the parent.
    foreign_key: String,
    /// Column on the parent table the FK matches against. Stored for
    /// the [`Relation`] impl; the inner builder doesn't need it
    /// directly (the FK constraint runs against the already-extracted
    /// parent value).
    parent_key: String,
    /// Pre-filtered builder against the child table.
    inner: Builder<R>,
    /// PhantomData carries the parent type so the [`Relation`] impl
    /// can name `type Parent = L` without a runtime field. `fn() -> L`
    /// keeps the type covariant + `Send + Sync` regardless of `L`.
    _phantom: PhantomData<fn() -> L>,
}

// The `R: Model` bound on the impl block re-elaborates Model's
// supertrait where-clauses for the same reason `Builder<M: Model>` in
// `builder.rs` and `HasOne<L, R>` in `has_one.rs` do: Rust's trait
// elaboration doesn't transitively propagate associated-type bounds
// from a supertrait's where clause to methods on a downstream `impl`.
// Without them, calling `R::query().filter(...)` inside `__new` fails
// because `Builder<R>` can't prove its own bounds are satisfied for
// an arbitrary `R: Model`. See `has_one.rs` for the long-form
// explanation.
impl<L, R> HasMany<L, R>
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
    /// Construct a `HasMany` from the parent row's key value + the FK
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

    /// Chainable `WHERE col = val` on the inner builder. Same shape
    /// as [`Builder::filter`]; the dual-API alias [`Self::db_where`]
    /// forwards here.
    pub fn filter(mut self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.inner = self.inner.filter(col, val);
        self
    }

    /// Laravel-shape alias for [`Self::filter`].
    pub fn db_where(self, col: impl IntoColumn, val: impl IntoVal) -> Self {
        self.filter(col, val)
    }

    /// Chainable `ORDER BY col <dir>` on the inner builder.
    pub fn order_by(mut self, col: impl IntoColumn, dir: Direction) -> Self {
        self.inner = self.inner.order_by(col, dir);
        self
    }

    /// `ORDER BY created_at DESC` — Laravel-shape sugar.
    ///
    /// Resolves only against models that declare a `created_at`
    /// column (which `#[suprnova::model]` auto-adds when timestamps
    /// are on, the default). Callers ordering by a non-timestamp
    /// column should use [`Self::order_by`] directly.
    pub fn latest(self) -> Self {
        self.order_by("created_at", Direction::Desc)
    }

    /// `ORDER BY created_at ASC` — Laravel-shape sugar. See
    /// [`Self::latest`] for the timestamp-column caveat.
    pub fn oldest(self) -> Self {
        self.order_by("created_at", Direction::Asc)
    }

    /// `LIMIT n` on the inner builder.
    pub fn limit(mut self, n: u64) -> Self {
        self.inner = self.inner.limit(n);
        self
    }

    /// Laravel-shape alias for [`Self::limit`].
    pub fn take(self, n: u64) -> Self {
        self.limit(n)
    }

    /// Execute the inner builder and return the first matching row.
    /// Returns `None` when the child table has no row pointing at
    /// this parent. Equivalent to `self.get().await?.first().cloned()`
    /// but issues a `LIMIT 1` so it's strictly cheaper for the
    /// 0-or-many shape.
    pub async fn first(self) -> Result<Option<R>, FrameworkError> {
        self.inner.first().await
    }

    /// Execute the inner builder and return every matching child row.
    ///
    /// Returns a [`Collection<R>`](crate::eloquent::Collection) — the
    /// Laravel-shaped wrapper around `Vec<R>`. Slice-shape access
    /// (`.iter()`, `.len()`, indexing) works directly via
    /// `Deref<Target = [R]>`; call sites that need an owned `Vec`
    /// reach for `.into_vec()`. The model-aware surface composes:
    /// `parent.children().get().await?.pluck::<String>("name")`.
    pub async fn get(self) -> Result<Collection<R>, FrameworkError> {
        self.inner.get().await
    }

    /// Count children. Returns `i64` to match the inner
    /// [`Builder::count`] surface — the per-row
    /// `<rel>_count() -> u64` cache accessor lives on the parent
    /// struct (populated by `__count_relation` at eager-load time).
    pub async fn count(self) -> Result<i64, FrameworkError> {
        self.inner.count().await
    }
}

/// Soft-delete scope modifiers for `HasMany<L, R>` where `R` is a
/// soft-delete model. See [`HasOne`](super::HasOne)'s identical impl
/// block for the rationale: forwards `with_trashed` / `only_trashed`
/// to the inner `Builder<R>`. Without this, `user.posts().get()`
/// would always be locked to alive children.
impl<L, R> HasMany<L, R>
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
    /// Widen the relation to include trashed children.
    pub fn with_trashed(mut self) -> Self {
        self.inner = self.inner.with_trashed();
        self
    }

    /// Restrict the relation to *only* trashed children.
    pub fn only_trashed(mut self) -> Self {
        self.inner = self.inner.only_trashed();
        self
    }
}

impl<L, R> Relation for HasMany<L, R>
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
    const KIND: RelationKind = RelationKind::HasMany;

    fn parent_key(&self) -> &str {
        &self.parent_key
    }

    fn foreign_key(&self) -> &str {
        &self.foreign_key
    }
}
