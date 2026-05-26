//! `HasManyThrough` / `HasOneThrough` — two-hop relations.
//!
//! Mirrors Laravel's
//! [`hasManyThrough`](https://laravel.com/docs/12.x/eloquent-relationships#has-many-through)
//! and [`hasOneThrough`](https://laravel.com/docs/12.x/eloquent-relationships#has-one-through)
//! semantics: traverse `A -> B -> C` where `B` is an intermediate
//! model whose FK column points at `A`, and `C` is the final target
//! whose FK column points at `B`.
//!
//! ## Soft-delete interaction (v1)
//!
//! Through relations use raw `INNER JOIN` SQL rather than the
//! `Builder<C>` pipeline, so the global soft-delete scope that
//! `C::query()` would install (`WHERE c.deleted_at IS NULL`) is
//! **not** applied. Likewise for the intermediate `B`. This means:
//!
//! - A `HasManyThrough<Country, User, Post>` where `Post` is
//!   soft-deletable will return trashed posts alongside alive ones.
//! - If `User` (the intermediate) is soft-deletable, trashed users
//!   still participate in the JOIN.
//!
//! This diverges from Laravel's `hasManyThrough` (which DOES filter
//! the intermediate and the target by `deleted_at IS NULL` when those
//! models declare `SoftDeletes`). Pinning the current behaviour in
//! `eloquent_soft_deletes_relations.rs` keeps a regression-tracker on
//! the gap; the proper fix lives in P12 (Through SQL stitches in the
//! deleted_at filters when both `B` and `C` carry the trait bound, via
//! a sealed `MaybeSoftDelete` blanket trait the macro implements for
//! every model).
//!
//! Until then, callers needing scoped Through reads should fall back
//! to chaining the two relations explicitly:
//!
//! ```ignore
//! // Instead of `country.posts().get()`, do:
//! let users = country.users().get().await?;
//! let user_ids: Vec<i64> = users.iter().map(|u| u.id).collect();
//! let posts = Post::query().filter_in("user_id", user_ids).get().await?;
//! // Both User and Post scopes apply to their respective query.
//! ```
//!
//! Laravel example: `Country` has many `User`s and `User` has many
//! `Post`s. `Country::posts()` is a HasManyThrough that returns every
//! `Post` belonging to any `User` in that country — a two-hop traversal.
//!
//! Default key conventions:
//!
//! - `first_key` (column on `B` pointing at `A`): `<snake(A)>_id`
//! - `second_key` (column on `C` pointing at `B`): `<snake(B)>_id`
//! - `local_key` (column on `A` matched by `first_key`): `"id"`
//! - `second_local_key` (column on `B` matched by `second_key`): `"id"`
//!
//! All four customisable via the macro's `first_key = "..."` /
//! `second_key = "..."` / `lk = "..."` / `second_local_key = "..."`
//! options. The chainable [`HasManyThrough::second_local_key`] setter
//! is also available at runtime — useful for tooling that constructs
//! the relation outside the `#[suprnova::model]` declaration.
//!
//! The terminal `.get()` / `.first()` / `.count()` issue a single
//! `INNER JOIN` query — one round trip per call, regardless of fan-out.
//! Eager loading is split across two queries (see the macro-emitted
//! dispatcher arm in `suprnova-macros/src/model/relations.rs`) to keep
//! the SeaORM deserialisation path homogeneous: the framework
//! deserialises `C` rows via the existing `Builder<C>` pipeline rather
//! than a raw JSON-row split.

use std::marker::PhantomData;

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

use crate::database::DB;
use crate::eloquent::EloquentModel;
use crate::eloquent::collection::Collection;
use crate::eloquent::model::{Model, json_value_to_sea_value};
use crate::eloquent::relations::{Relation, RelationKind};
use crate::error::FrameworkError;

/// Two-hop one-to-many relation from parent `A` through intermediate
/// `B` to target `C`. Constructed by the macro-emitted relation method
/// (`fn posts(&self) -> HasManyThrough<Self, User, Post>`); user code
/// never calls [`HasManyThrough::__new`] directly.
///
/// The wrapper carries the key metadata + the parent's local-key
/// value, all paid up at construction time. Terminal methods
/// (`get`, `first`, `count`) issue the SQL.
pub struct HasManyThrough<A, B, C>
where
    A: EloquentModel,
    B: EloquentModel,
    C: Model,
    C: From<<C::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <C::Entity as sea_orm::EntityTrait>::Model: From<C>
        + sea_orm::IntoActiveModel<<C::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <C::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<C::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Parent row's local-key value, JSON-encoded. Matches the rest of
    /// the relation surface — `HasMany`, `HasOne`, `BelongsToMany` all
    /// store the parent key as `serde_json::Value` so the runtime path
    /// stays homogeneous regardless of the PK shape (`i64`, `String`,
    /// `Uuid`-via-string). The conversion to `sea_orm::Value` happens
    /// at the SQL binding boundary via [`json_value_to_sea_value`].
    parent_key_value: serde_json::Value,
    /// Column on `B` pointing at `A`. Default: `<snake(A)>_id`.
    first_key: String,
    /// Column on `C` pointing at `B`. Default: `<snake(B)>_id`.
    second_key: String,
    /// Column on `A` matched by `first_key`. Default: `"id"`. Only
    /// affects the [`Relation::parent_key`] metadata — the runtime
    /// value was already extracted at construction.
    local_key: String,
    /// Column on `B` matched by `second_key`. Default: `"id"`. Drives
    /// the `INNER JOIN ... ON C.{second_key} = B.{second_local_key}`
    /// predicate.
    second_local_key: String,
    /// PhantomData carries `A`, `B`, `C` so the [`Relation`] impl can
    /// name `type Parent = A` / `type Target = C` without runtime
    /// fields. `fn() -> (A, B, C)` keeps the type covariant +
    /// `Send + Sync` regardless of the parameters.
    #[allow(clippy::type_complexity)]
    _phantom: PhantomData<fn() -> (A, B, C)>,
}

impl<A, B, C> HasManyThrough<A, B, C>
where
    A: EloquentModel,
    B: EloquentModel,
    C: Model,
    C: From<<C::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <C::Entity as sea_orm::EntityTrait>::Model: From<C>
        + sea_orm::IntoActiveModel<<C::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <C::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<C::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Construct a `HasManyThrough`. Invoked by the macro-emitted
    /// relation method; not part of the public API.
    ///
    /// `parent_key_value` is the JSON-serialised parent PK
    /// (e.g. `serde_json::to_value(&self.id)`). `first_key` and
    /// `second_key` come from macro-resolved defaults or user-supplied
    /// `first_key = "..."` / `second_key = "..."` options.
    #[doc(hidden)]
    pub fn __new(
        parent_key_value: serde_json::Value,
        first_key: String,
        second_key: String,
    ) -> Self {
        Self {
            parent_key_value,
            first_key,
            second_key,
            local_key: "id".into(),
            second_local_key: "id".into(),
            _phantom: PhantomData,
        }
    }

    /// Override the column on `B` pointing at `A`.
    pub fn first_key(mut self, key: impl Into<String>) -> Self {
        self.first_key = key.into();
        self
    }

    /// Override the column on `C` pointing at `B`.
    pub fn second_key(mut self, key: impl Into<String>) -> Self {
        self.second_key = key.into();
        self
    }

    /// Override the column on `A` matched by `first_key`. Only updates
    /// metadata — the runtime parent value was extracted at
    /// construction. Mirrors the [`crate::eloquent::HasMany::local_key`]
    /// setter shape.
    pub fn local_key(mut self, key: impl Into<String>) -> Self {
        self.local_key = key.into();
        self
    }

    /// Override the column on `B` matched by `second_key`. Drives the
    /// `INNER JOIN ... ON C.{second_key} = B.{second_local_key}`
    /// predicate.
    pub fn second_local_key(mut self, key: impl Into<String>) -> Self {
        self.second_local_key = key.into();
        self
    }

    /// Fetch every `C` row reachable from this parent through `B`.
    ///
    /// Issues a single `INNER JOIN` query:
    ///
    /// ```sql
    /// SELECT c.*
    ///   FROM <C> c
    ///  INNER JOIN <B> b
    ///     ON c.<second_key> = b.<second_local_key>
    ///  WHERE b.<first_key> = ?
    /// ```
    ///
    /// Backend-aware placeholders (`?` for sqlite / mysql, `$1` for
    /// postgres) match the rest of the framework's raw-SQL paths.
    pub async fn get(self) -> Result<Collection<C>, FrameworkError> {
        let conn = DB::connection()?;
        let backend = conn.inner().get_database_backend();
        let ph = match backend {
            DatabaseBackend::Postgres => "$1".to_string(),
            _ => "?".to_string(),
        };
        let b_table = <B as EloquentModel>::TABLE;
        let c_table = <C as EloquentModel>::TABLE;
        let sql = format!(
            "SELECT {c}.* FROM {c} INNER JOIN {b} ON {c}.{second_key} = {b}.{second_local_key} \
             WHERE {b}.{first_key} = {ph}",
            c = c_table,
            b = b_table,
            second_key = self.second_key,
            second_local_key = self.second_local_key,
            first_key = self.first_key,
            ph = ph,
        );
        let stmt = Statement::from_sql_and_values(
            backend,
            &sql,
            vec![json_value_to_sea_value(&self.parent_key_value)],
        );
        use sea_orm::EntityTrait;
        let rows = <C as EloquentModel>::Entity::find()
            .from_raw_sql(stmt)
            .all(conn.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(Collection::from_vec(
            rows.into_iter().map(C::from).collect(),
        ))
    }

    /// Convenience over `.get()` — drop everything after the first row.
    pub async fn first(self) -> Result<Option<C>, FrameworkError> {
        Ok(self.get().await?.into_vec().into_iter().next())
    }

    /// `SELECT COUNT(*) FROM <C> INNER JOIN <B> ... WHERE B.<first_key> = ?`.
    /// Returns `i64` to match [`crate::eloquent::HasMany::count`] and
    /// [`crate::eloquent::BelongsToMany::count`].
    pub async fn count(self) -> Result<i64, FrameworkError> {
        let conn = DB::connection()?;
        let backend = conn.inner().get_database_backend();
        let ph = match backend {
            DatabaseBackend::Postgres => "$1".to_string(),
            _ => "?".to_string(),
        };
        let b_table = <B as EloquentModel>::TABLE;
        let c_table = <C as EloquentModel>::TABLE;
        let sql = format!(
            "SELECT COUNT(*) AS __sn_count FROM {c} INNER JOIN {b} \
             ON {c}.{second_key} = {b}.{second_local_key} \
             WHERE {b}.{first_key} = {ph}",
            c = c_table,
            b = b_table,
            second_key = self.second_key,
            second_local_key = self.second_local_key,
            first_key = self.first_key,
            ph = ph,
        );
        let stmt = Statement::from_sql_and_values(
            backend,
            &sql,
            vec![json_value_to_sea_value(&self.parent_key_value)],
        );
        let row = conn
            .inner()
            .query_one(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(row
            .and_then(|r| r.try_get::<i64>("", "__sn_count").ok())
            .unwrap_or(0))
    }
}

impl<A, B, C> Relation for HasManyThrough<A, B, C>
where
    A: EloquentModel,
    B: EloquentModel,
    C: Model,
    C: From<<C::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <C::Entity as sea_orm::EntityTrait>::Model: From<C>
        + sea_orm::IntoActiveModel<<C::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <C::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<C::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    type Parent = A;
    type Target = C;
    const KIND: RelationKind = RelationKind::HasManyThrough;

    fn parent_key(&self) -> &str {
        &self.local_key
    }

    fn foreign_key(&self) -> &str {
        &self.first_key
    }
}

/// Two-hop one-to-one relation. Same key mechanics as
/// [`HasManyThrough`] but the terminal methods return at most one row.
///
/// Internally delegates to [`HasManyThrough`] and takes the first row;
/// the wrapper exists so the [`Relation::KIND`] constant resolves to
/// [`RelationKind::HasOneThrough`] for admin tooling and dispatchers
/// that branch on kind.
pub struct HasOneThrough<A, B, C>
where
    A: EloquentModel,
    B: EloquentModel,
    C: Model,
    C: From<<C::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <C::Entity as sea_orm::EntityTrait>::Model: From<C>
        + sea_orm::IntoActiveModel<<C::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <C::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<C::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    inner: HasManyThrough<A, B, C>,
}

impl<A, B, C> HasOneThrough<A, B, C>
where
    A: EloquentModel,
    B: EloquentModel,
    C: Model,
    C: From<<C::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <C::Entity as sea_orm::EntityTrait>::Model: From<C>
        + sea_orm::IntoActiveModel<<C::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <C::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<C::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Construct a `HasOneThrough`. Invoked by the macro-emitted
    /// relation method; not part of the public API.
    #[doc(hidden)]
    pub fn __new(
        parent_key_value: serde_json::Value,
        first_key: String,
        second_key: String,
    ) -> Self {
        Self {
            inner: HasManyThrough::__new(parent_key_value, first_key, second_key),
        }
    }

    /// Override the column on `B` pointing at `A`.
    pub fn first_key(mut self, key: impl Into<String>) -> Self {
        self.inner = self.inner.first_key(key);
        self
    }

    /// Override the column on `C` pointing at `B`.
    pub fn second_key(mut self, key: impl Into<String>) -> Self {
        self.inner = self.inner.second_key(key);
        self
    }

    /// Override the column on `A` matched by `first_key`.
    pub fn local_key(mut self, key: impl Into<String>) -> Self {
        self.inner = self.inner.local_key(key);
        self
    }

    /// Override the column on `B` matched by `second_key`.
    pub fn second_local_key(mut self, key: impl Into<String>) -> Self {
        self.inner = self.inner.second_local_key(key);
        self
    }

    /// Fetch the first matching `C` row reachable from this parent.
    ///
    /// Equivalent to `.get()` for HasOne semantics — at most one row.
    pub async fn first(self) -> Result<Option<C>, FrameworkError> {
        self.inner.first().await
    }

    /// Fetch the matching `C` row (HasOne semantics — at most one row).
    /// Returns `None` when no `C` row is reachable.
    pub async fn get(self) -> Result<Option<C>, FrameworkError> {
        Ok(self.inner.get().await?.into_vec().into_iter().next())
    }
}

impl<A, B, C> Relation for HasOneThrough<A, B, C>
where
    A: EloquentModel,
    B: EloquentModel,
    C: Model,
    C: From<<C::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <C::Entity as sea_orm::EntityTrait>::Model: From<C>
        + sea_orm::IntoActiveModel<<C::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <C::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<C::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    type Parent = A;
    type Target = C;
    const KIND: RelationKind = RelationKind::HasOneThrough;

    fn parent_key(&self) -> &str {
        self.inner.parent_key()
    }

    fn foreign_key(&self) -> &str {
        self.inner.foreign_key()
    }
}
