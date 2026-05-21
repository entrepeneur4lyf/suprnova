//! Polymorphic relations — `MorphTo` / `MorphOne` / `MorphMany`.
//!
//! `MorphTo` lives on the morph-table side (e.g. `Comment.commentable`):
//! a polymorphic FK column pair (`commentable_id` + `commentable_type`)
//! that points at a row in one of several "parent" tables. Because the
//! parent type varies per row, the macro emits a per-family enum
//! (`CommentableMorph { MorphPost(MorphPost), MorphVideo(MorphVideo),
//! Unknown(String, i64) }`) at the declaration site. The runtime
//! [`MorphTo<C>`] struct in this module is only a metadata carrier for
//! the `RelationEntry` inventory + the user-side `pub use` re-export;
//! it does NOT participate in the per-family dispatch (that happens in
//! the macro-generated `<Name>MorphFetch::get` async method).
//!
//! [`MorphOne`] / [`MorphMany`] live on the parent side. They mirror
//! [`HasOne`](super::HasOne) / [`HasMany`](super::HasMany) but layer
//! the morph-type discriminator on top — the inner [`Builder<R>`] is
//! pre-filtered with both `<morph>_id = <parent_id>` and
//! `<morph>_type = <parent_morph_type>`, so polymorphic children
//! pointing at OTHER families never appear in `.get()` / `.first()` /
//! `.count()` results.
//!
//! Eager-load orchestration lives in the parent model's
//! `__eager_load("<rel>", ...)` match arm; this module's structs only
//! handle the lazy `.first()` / `.get()` / `.count()` path. The
//! parent-side relation method also passes the parent's
//! `morph_type = "..."` attribute value into the morph runtime (so the
//! filter knows which type-string to send).

use std::marker::PhantomData;

use crate::eloquent::builder::{Builder, Direction, IntoColumn, IntoVal};
use crate::eloquent::collection::Collection;
use crate::eloquent::model::Model;
use crate::eloquent::relations::{Relation, RelationKind};
use crate::eloquent::EloquentModel;
use crate::error::FrameworkError;

/// One-to-many morph relation from parent `L` to children `R`. The
/// parent declares this as `MorphMany<Child> { name = "..." }`, e.g.
/// `comments: MorphMany<Comment> { name = "commentable" }` on `Post`
/// + `Video`.
///
/// Constructed by the macro-emitted relation method
/// (`fn comments(&self) -> MorphMany<Self, Comment>`); user code never
/// calls [`MorphMany::__new`] directly.
///
/// The wrapper carries the parent PK value, the morph-name (which
/// controls the `<name>_id` + `<name>_type` column names), the
/// parent's morph-type string, and a pre-filtered [`Builder<R>`]
/// targeting the child table. Chaining `filter` / `order_by` / `limit`
/// forwards onto that builder.
pub struct MorphMany<L, R>
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
    /// Parent row's PK value, JSON-encoded. Same reasoning as
    /// [`HasMany`](super::HasMany) — JSON is the
    /// [`Builder::filter`] storage shape, and converting once at
    /// construction keeps the wrapper's chainable surface free of
    /// `T: IntoVal` bounds.
    ///
    /// Stored on the struct (rather than consumed at construction)
    /// because future overrides like `.parent_key(...)` would need to
    /// rebuild the inner builder, and admin tooling reading the
    /// [`Relation`] surface needs to surface this value alongside the
    /// morph metadata.
    #[allow(dead_code)]
    parent_key_value: serde_json::Value,
    /// Morph family name — e.g. `"commentable"`. Controls both the
    /// `<name>_id` and `<name>_type` column names on the child table.
    /// Read by the [`Relation`] impl + the eager-load dispatcher.
    morph_name: String,
    /// What the PARENT registers as in the child's `<name>_type`
    /// column. Defaults to `to_snake(struct_name)` at the macro
    /// emission site; can be overridden per-struct via
    /// `#[model(morph_type = "...")]`. Stored for the [`Relation`]
    /// impl + admin introspection; consumed once at construction
    /// (cloned into the inner builder's WHERE clause).
    #[allow(dead_code)]
    morph_type_value: String,
    /// Pre-filtered builder against the child table — both
    /// `<name>_id = <parent_id>` AND `<name>_type = <morph_type>`
    /// applied at construction.
    inner: Builder<R>,
    /// PhantomData carries the parent type so the [`Relation`] impl
    /// can name `type Parent = L` without a runtime field.
    _phantom: PhantomData<fn() -> L>,
}

impl<L, R> MorphMany<L, R>
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
    /// Construct a `MorphMany` from the parent row's PK value + morph
    /// name + the parent's morph-type string. Invoked by the macro-
    /// emitted relation method; not part of the public API.
    ///
    /// `morph_name` controls the `<name>_id` + `<name>_type` columns.
    /// `morph_type_value` is the string the parent expects to see in
    /// the child's `<name>_type` column.
    #[doc(hidden)]
    pub fn __new(
        parent_key_value: serde_json::Value,
        morph_name: String,
        morph_type_value: String,
    ) -> Self {
        let id_col = format!("{morph_name}_id");
        let type_col = format!("{morph_name}_type");
        // Builder::filter() takes anything `IntoVal`, which is the same
        // JSON-shaped path HasMany / HasOne use — we wrap the
        // type-string in `serde_json::Value::String` so the inner
        // WhereTerm storage stays homogeneous with the rest of the
        // dual-API.
        let type_val = serde_json::Value::String(morph_type_value.clone());
        let inner = R::query()
            .filter(id_col.as_str(), parent_key_value.clone())
            .filter(type_col.as_str(), type_val);
        Self {
            parent_key_value,
            morph_name,
            morph_type_value,
            inner,
            _phantom: PhantomData,
        }
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

    /// `ORDER BY created_at DESC` — Laravel-shape sugar. Only resolves
    /// against children that declare a `created_at` column.
    pub fn latest(self) -> Self {
        self.order_by("created_at", Direction::Desc)
    }

    /// `ORDER BY created_at ASC` — Laravel-shape sugar.
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

    /// Execute and return the first matching child row.
    pub async fn first(self) -> Result<Option<R>, FrameworkError> {
        self.inner.first().await
    }

    /// Execute and return every matching child row.
    ///
    /// Returns a [`Collection<R>`](crate::eloquent::Collection); see
    /// [`HasMany::get`](super::HasMany::get) for return-type
    /// rationale.
    pub async fn get(self) -> Result<Collection<R>, FrameworkError> {
        self.inner.get().await
    }

    /// Count children. Returns `i64` to match the inner
    /// [`Builder::count`] surface. Server-side aggregation through the
    /// builder — no client-side row buffering.
    pub async fn count(self) -> Result<i64, FrameworkError> {
        self.inner.count().await
    }
}

/// Soft-delete forwarding for `MorphMany<L, R>` when `R: SoftDeletes`.
/// Mirrors [`HasMany`](super::HasMany)'s equivalent block — both wrap
/// an inner `Builder<R>` and need only one-liner forwarding to the
/// underlying `Builder::with_trashed` / `only_trashed`.
impl<L, R> MorphMany<L, R>
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

impl<L, R> Relation for MorphMany<L, R>
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
    const KIND: RelationKind = RelationKind::MorphMany;

    fn parent_key(&self) -> &str {
        // Polymorphic relations always join the parent's PK (`id`)
        // against the child's `<name>_id` column. The macro doesn't
        // currently expose a parent-key override on morph relations
        // (the morph runtime keys are baked into the column-name
        // construction in `__new`); if a non-`id` parent PK is needed
        // the parent model declares it via `primary_key = "..."` and
        // the macro reads that when populating the inner builder, not
        // through this accessor.
        "id"
    }

    fn foreign_key(&self) -> &str {
        // Surface the morph name as the "foreign key" name — admin
        // tooling reading the [`RelationEntry`](super::RelationEntry)
        // surfaces this as the child-side column root; the actual
        // column is `<morph_name>_id`. The morph-type discriminator
        // (`<morph_name>_type`) is implicit.
        &self.morph_name
    }
}

/// Single-row morph relation from parent `L` to child `R`. Same shape
/// as [`MorphMany`] internally — pre-filtered builder with both
/// `<name>_id` and `<name>_type` predicates applied — but the public
/// surface returns `Option<R>` from `.first()` rather than a Vec from
/// `.get()`.
///
/// Constructed by the macro-emitted relation method
/// (`fn profile_image(&self) -> MorphOne<Self, Image>`); user code
/// never calls [`MorphOne::__new`] directly.
pub struct MorphOne<L, R>
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
    inner: MorphMany<L, R>,
}

impl<L, R> MorphOne<L, R>
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
    /// Construct a `MorphOne`. Invoked by the macro-emitted relation
    /// method.
    #[doc(hidden)]
    pub fn __new(
        parent_key_value: serde_json::Value,
        morph_name: String,
        morph_type_value: String,
    ) -> Self {
        Self {
            inner: MorphMany::__new(parent_key_value, morph_name, morph_type_value),
        }
    }

    /// Chainable `WHERE col = val` on the inner builder.
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

    /// Execute and return the single matching child row (if any).
    pub async fn first(self) -> Result<Option<R>, FrameworkError> {
        self.inner.first().await
    }
}

/// Soft-delete forwarding for `MorphOne<L, R>` when `R: SoftDeletes`.
/// Same forwarding pattern as the [`MorphMany`] block above, applied
/// through the inner `MorphMany`'s own `with_trashed` / `only_trashed`.
impl<L, R> MorphOne<L, R>
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
    /// Widen the relation to include the trashed child if it exists.
    pub fn with_trashed(mut self) -> Self {
        self.inner = self.inner.with_trashed();
        self
    }

    /// Restrict the relation to a trashed child.
    pub fn only_trashed(mut self) -> Self {
        self.inner = self.inner.only_trashed();
        self
    }
}

impl<L, R> Relation for MorphOne<L, R>
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
    const KIND: RelationKind = RelationKind::MorphOne;

    fn parent_key(&self) -> &str {
        self.inner.parent_key()
    }

    fn foreign_key(&self) -> &str {
        self.inner.foreign_key()
    }
}

/// Inverse-side morph relation. Lives on the morph-table side
/// (e.g. `Comment.commentable`); the user declares it as
/// `MorphTo { name = "commentable", targets = [Post, Video] }`.
///
/// **Metadata only.** Unlike [`HasOne`](super::HasOne) /
/// [`BelongsTo`](super::BelongsTo), the user's call site DOES NOT
/// receive a `MorphTo<C>` instance directly. Instead the macro emits
/// a per-family enum (`CommentableMorph`) and a fetch helper
/// (`CommentableMorphFetch`) at the declaration site, and the
/// `comment.commentable()` method returns the fetch helper. The
/// `MorphTo<C>` struct exists so:
///
/// 1. The [`RelationEntry`](super::RelationEntry) inventory submission
///    can name a concrete type (the per-family enum is local to the
///    declaration site and isn't reachable from the inventory entry).
/// 2. The framework re-exports a symbol users can name for advanced
///    cases (custom relation impls, third-party integrations).
/// 3. The seal contract through `Relation` stays uniform — every
///    declared relation has an impl.
///
/// The per-family enum dispatch in `<Name>MorphFetch::get()` calls
/// `Target::find(id)` for each branch directly; it does not flow
/// through this struct.
///
/// # v1 restriction: `i64`-only morph IDs
///
/// `MorphTo::morph_id` is hardcoded to `i64`. Polymorphic targets
/// must therefore use `i64` primary keys, and the morph table's
/// `<name>_id` column must also be `i64`. Models whose primary key
/// is `String` or a UUID-as-string cannot be `MorphTo` targets in
/// v1 — the per-family fetch helper calls
/// `<Target as Model>::find(self.morph_id)` with an `i64`, which
/// will fail to type-check at the user's call site against a target
/// whose `Model::Key` is anything other than `i64`.
///
/// v2 will parameterise `morph_id` on the FK column type so the
/// morph machinery accepts the full PK shape lattice (`i64` /
/// `String` / `Uuid`). Until then, declare polymorphic models with
/// `i64` primary keys.
pub struct MorphTo<C>
where
    C: EloquentModel,
{
    /// FK value on the child row (`<name>_id` column).
    pub morph_id: i64,
    /// Type-string on the child row (`<name>_type` column).
    pub morph_type: String,
    _phantom: PhantomData<fn() -> C>,
}

impl<C> MorphTo<C>
where
    C: EloquentModel,
{
    /// Construct a `MorphTo` metadata carrier. Invoked by macro-
    /// emitted code only — user code uses the per-family fetch helper
    /// instead of touching this struct directly.
    #[doc(hidden)]
    pub fn __new(morph_id: i64, morph_type: String) -> Self {
        Self {
            morph_id,
            morph_type,
            _phantom: PhantomData,
        }
    }
}

impl<C> Relation for MorphTo<C>
where
    C: EloquentModel,
{
    type Parent = C;
    /// `MorphTo` doesn't have a single concrete target — the per-family
    /// enum at the declaration site stands in. The unit type signals
    /// "look at the macro-generated per-family enum, not a single
    /// target" to admin tooling and the eager-load dispatcher.
    type Target = ();
    const KIND: RelationKind = RelationKind::MorphTo;

    fn parent_key(&self) -> &str {
        "id"
    }

    fn foreign_key(&self) -> &str {
        ""
    }
}
