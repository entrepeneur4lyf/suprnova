//! Polymorphic many-to-many — m2m through a pivot table that
//! distinguishes parent rows by a `*_type` discriminator column.
//!
//! Mirrors Laravel's
//! [polymorphic m2m](https://laravel.com/docs/12.x/eloquent-relationships#many-to-many-polymorphic-relations)
//! semantics: a single pivot table (`taggables`) carries one FK to the
//! shared m2m side (`tag_id`) plus a `<name>_id` / `<name>_type` pair
//! that points at one of several parent morph families (Post, Video).
//!
//! Two flavours:
//!
//! - [`MorphToMany`] lives on the morphable side (`Post.tags()`).
//!   The pivot row matches `<name>_id = parent.id AND
//!   <name>_type = parent_morph_type`.
//! - [`MorphedByMany`] is the inverse, on the m2m side
//!   (`Tag.posts()` / `Tag.videos()`). The pivot row matches
//!   `<pivot_foreign_key> = tag.id AND <name>_type = target_morph_type`.
//!   Filters one specific target morph family at a time so
//!   `tag.posts()` returns only Post-typed taggables and `tag.videos()`
//!   returns only Video-typed taggables, never mixing them in a single
//!   collection.
//!
//! Both share the [`BelongsToMany`](super::BelongsToMany) two-query
//! load strategy (related rows by IN + pivot rows separately), with
//! the extra `*_type` filter layered on every SQL statement that
//! touches the pivot.
//!
//! Default key conventions:
//!
//! - `morph_name` (controls the `<name>_id` + `<name>_type` columns):
//!   the relation name itself (`"taggable"` from
//!   `relations = { taggable: ... }`).
//! - `pivot_table`: `<P as EloquentModel>::TABLE` — the pivot model's
//!   own `#[model(table = "...")]` declaration is the single source of
//!   truth.
//! - `pivot_related_key` (`MorphToMany`'s pivot column → R): `<snake(R)>_id`.
//! - `pivot_foreign_key` (`MorphedByMany`'s pivot column → L=Tag):
//!   `<snake(L)>_id`.
//! - `parent_morph_type` (`MorphToMany`): L's `morph_type = "..."`
//!   attribute, defaulted to `to_snake(struct_name)`.
//! - `target_morph_type` (`MorphedByMany`): R's `morph_type` — passed
//!   explicitly via the relation declaration's `target_morph_type =
//!   "..."` option, since the macro at the L-side declaration site
//!   can't introspect R's `morph_type` attribute (it lives in a
//!   separate `#[suprnova::model]` invocation).
//!
//! Mutators (`MorphToMany` only):
//!
//! - [`attach`](MorphToMany::attach) — INSERT a pivot row with the
//!   `<name>_id` + `<name>_type` + pivot related FK.
//! - [`attach_with`](MorphToMany::attach_with) — INSERT with extra
//!   pivot columns (and timestamps if `with_timestamps()` is set).
//! - [`detach`](MorphToMany::detach) — DELETE matching the parent's id
//!   + type.
//! - [`sync`](MorphToMany::sync) — diff-and-apply, transactional via
//!   `DatabaseConnection::begin()`.
//!
//! Readers (both flavours):
//!
//! - `.get()` — JOIN R to pivot with the type filter. Two-query
//!   strategy filling `__pivot` per row.
//! - `.first()` — `.get().into_iter().next()`.
//! - `.count()` — `SELECT COUNT(*) FROM pivot WHERE ... AND
//!   <name>_type = ?`.
//!
//! Eager loading happens through the parent model's `__eager_load`
//! match arm — emitted by `#[suprnova::model]` and exercised by
//! `MmPost::with(["tags"])`. Same per-attachment clone semantics as
//! BelongsToMany (each parent gets its own `__pivot` context on every
//! returned R clone).

use std::marker::PhantomData;
use std::sync::Arc;

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};

use crate::database::transaction::ExecutorChoice;
use crate::eloquent::attrs::Attrs;
use crate::eloquent::builder::Builder;
use crate::eloquent::collection::Collection;
use crate::eloquent::model::{json_value_to_sea_value, Model};
use crate::eloquent::relations::{Relation, RelationKind};
use crate::eloquent::EloquentModel;
use crate::error::FrameworkError;

/// Boxed builder-rewrite closure for [`MorphToMany::with_trashed`] /
/// [`MorphedByMany::with_trashed`] (and their `only_trashed`
/// siblings). Same closure-erasure trick as
/// [`super::belongs_to::ScopeRewrite`][crate::eloquent::relations::belongs_to].
type ScopeRewrite<R> = Box<dyn FnOnce(Builder<R>) -> Builder<R> + Send>;

/// Polymorphic m2m from morphable parent `L` to m2m target `R` through
/// polymorphic pivot `P`. Constructed by the macro-emitted relation
/// method (`fn tags(&self) -> MorphToMany<Self, Tag, Taggable>`); user
/// code never calls [`MorphToMany::__new`] directly.
///
/// The wrapper holds the morph + key metadata plus the parent's PK
/// value and morph-type string, all paid up at construction time.
/// Terminal methods (`attach`, `detach`, `sync`, `get`, `first`,
/// `count`) issue the SQL.
pub struct MorphToMany<L, R, P>
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
    P: Model + 'static,
    P: From<<P::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <P::Entity as sea_orm::EntityTrait>::Model: From<P>
        + sea_orm::IntoActiveModel<<P::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <P::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<P::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Parent row's local-key value, JSON-encoded. The macro emits
    /// `serde_json::to_value(&self.id)` at the call site, matching
    /// [`BelongsToMany`](super::BelongsToMany)'s storage convention.
    parent_key_value: serde_json::Value,
    /// L's `morph_type = "..."` attribute value — the string the
    /// pivot's `<morph_name>_type` column has to equal for the pivot
    /// row to belong to this parent. Defaults to `to_snake(L)` at the
    /// macro emission site when the attribute isn't declared.
    parent_morph_type: String,
    /// Morph family name. Controls the `<morph_name>_id` and
    /// `<morph_name>_type` column names on the pivot table. Defaults
    /// to the relation name (e.g. `"taggable"` from a relation
    /// declared as `taggable: MorphToMany<...>`); overridable via
    /// `name = "..."` (alias `morph_name = "..."`).
    morph_name: String,
    /// Pivot table name. Defaults to `<P as EloquentModel>::TABLE` —
    /// the pivot's own `#[suprnova::model(table = "...")]` declaration
    /// is the single source of truth. Override via the macro's
    /// `pivot_table = "..."` option.
    pivot_table: String,
    /// Pivot column pointing at the related row (`R`). Default:
    /// `<snake(R)>_id`. Override via `pivot_related_key = "..."`.
    pivot_related_key: String,
    /// Parent table's key column. Default `"id"`. Honoured by the
    /// [`Relation`] impl + admin introspection.
    parent_key: String,
    /// Related table's primary-key COLUMN name used by the JOIN in
    /// [`Self::get`]. Default `"id"`. Set via `.related_pk(...)`.
    related_key: String,
    /// Extra pivot columns to project into `__pivot`. Always includes
    /// the implicit pivot FKs + the morph discriminator pair.
    pivot_columns: Vec<String>,
    /// When true, the attach path stamps `created_at` / `updated_at`
    /// on every pivot row written.
    with_timestamps: bool,
    /// Deferred soft-delete scope rewrite applied to the related-row
    /// query at [`Self::get`] / [`Self::first`] time. Only set by
    /// [`Self::with_trashed`] / [`Self::only_trashed`], both gated
    /// on `R: SoftDeletes`.
    scope_rewrite: Option<ScopeRewrite<R>>,
    /// `PhantomData` carries `L`, `R`, `P` so the [`Relation`] impl
    /// can name `type Parent = L` / `type Target = R` without runtime
    /// fields.
    #[allow(clippy::type_complexity)]
    _phantom: PhantomData<fn() -> (L, R, P)>,
}

impl<L, R, P> MorphToMany<L, R, P>
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
    P: Model + 'static,
    P: From<<P::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <P::Entity as sea_orm::EntityTrait>::Model: From<P>
        + sea_orm::IntoActiveModel<<P::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <P::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<P::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Construct a `MorphToMany`. Invoked by the macro-emitted
    /// relation method; not part of the public API.
    #[doc(hidden)]
    pub fn __new(
        parent_key_value: serde_json::Value,
        parent_morph_type: String,
        morph_name: String,
        pivot_table: String,
        pivot_related_key: String,
    ) -> Self {
        Self {
            parent_key_value,
            parent_morph_type,
            morph_name,
            pivot_table,
            pivot_related_key,
            parent_key: "id".into(),
            related_key: "id".into(),
            pivot_columns: Vec::new(),
            with_timestamps: false,
            scope_rewrite: None,
            _phantom: PhantomData,
        }
    }

    /// Declare extra pivot columns to surface on each loaded R via
    /// `r.pivot::<P>()`. Mirrors Laravel's `->withPivot([...])`.
    ///
    /// The morph FK + type columns are always loaded; this option is
    /// for "extras" — `assigned_at`, `notes`, custom payloads.
    pub fn with_pivot<I, S>(mut self, columns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.pivot_columns
            .extend(columns.into_iter().map(Into::into));
        self
    }

    /// Touch `created_at` / `updated_at` on every pivot row written
    /// by `attach` / `attach_with` / `sync`. Mirrors Laravel's
    /// `->withTimestamps()`.
    pub fn with_timestamps(mut self) -> Self {
        self.with_timestamps = true;
        self
    }

    /// Override the parent's key column. Only updates the metadata
    /// surface; the runtime parent value was extracted at construction.
    pub fn local_key(mut self, key: impl Into<String>) -> Self {
        self.parent_key = key.into();
        self
    }

    /// Override the related-side primary-key COLUMN name used by
    /// [`Self::get`]'s IN-set filter. Defaults to `"id"`. Set this
    /// when the related model declares a non-`id` primary key.
    pub fn related_pk(mut self, key: impl Into<String>) -> Self {
        self.related_key = key.into();
        self
    }

    /// Insert a pivot row linking the parent (`<morph_name>_id =
    /// parent.id AND <morph_name>_type = parent_morph_type`) to
    /// `related_id`. Equivalent to `attach_with(related_id,
    /// Attrs::new())`.
    pub async fn attach(
        self,
        related_id: impl Into<serde_json::Value>,
    ) -> Result<(), FrameworkError> {
        self.attach_with(related_id, Attrs::new()).await
    }

    /// Insert a pivot row with extra column values (and timestamps
    /// when `with_timestamps()` is on).
    pub async fn attach_with(
        self,
        related_id: impl Into<serde_json::Value>,
        extra: Attrs,
    ) -> Result<(), FrameworkError> {
        // Phase 10C audit-fix AF2 — resolve through ExecutorChoice so the
        // pivot INSERT lands on the ambient transaction when CURRENT_TX
        // is active.
        let exec = ExecutorChoice::resolve_write(None, None, None).await?;
        let backend = exec.backend();
        let id = related_id.into();
        match &exec {
            ExecutorChoice::Tx(t) => morph_attach_one(
                t.as_ref(),
                backend,
                &self.pivot_table,
                &self.morph_name,
                &self.pivot_related_key,
                &self.parent_key_value,
                &self.parent_morph_type,
                &id,
                extra,
                self.with_timestamps,
            )
            .await,
            ExecutorChoice::Pool(c) => morph_attach_one(
                c.inner(),
                backend,
                &self.pivot_table,
                &self.morph_name,
                &self.pivot_related_key,
                &self.parent_key_value,
                &self.parent_morph_type,
                &id,
                extra,
                self.with_timestamps,
            )
            .await,
        }
    }

    /// Delete pivot rows linking this parent to `related_id`.
    pub async fn detach(
        self,
        related_id: impl Into<serde_json::Value>,
    ) -> Result<(), FrameworkError> {
        // Phase 10C audit-fix AF2 — see attach_with above.
        let exec = ExecutorChoice::resolve_write(None, None, None).await?;
        let backend = exec.backend();
        let id = related_id.into();
        match &exec {
            ExecutorChoice::Tx(t) => morph_detach_one(
                t.as_ref(),
                backend,
                &self.pivot_table,
                &self.morph_name,
                &self.pivot_related_key,
                &self.parent_key_value,
                &self.parent_morph_type,
                &id,
            )
            .await,
            ExecutorChoice::Pool(c) => morph_detach_one(
                c.inner(),
                backend,
                &self.pivot_table,
                &self.morph_name,
                &self.pivot_related_key,
                &self.parent_key_value,
                &self.parent_morph_type,
                &id,
            )
            .await,
        }
    }

    /// Replace the parent's full set of attached relations with the
    /// given IDs. Transactional — partial failure rolls back.
    pub async fn sync<I, V>(self, ids: I) -> Result<(), FrameworkError>
    where
        I: IntoIterator<Item = V>,
        V: Into<serde_json::Value>,
    {
        use std::collections::{HashMap, HashSet};

        // Phase 10C audit-fix AF2 — same shape as BelongsToMany::sync —
        // route through ExecutorChoice so the SELECT + inner writes
        // honor CURRENT_TX.
        let exec = ExecutorChoice::resolve_write(None, None, None).await?;
        let backend = exec.backend();

        let mut seen_target: HashSet<String> = HashSet::new();
        let mut target_ids: Vec<serde_json::Value> = Vec::new();
        for raw in ids {
            let v: serde_json::Value = raw.into();
            let key = v.to_string();
            if seen_target.insert(key) {
                target_ids.push(v);
            }
        }

        let id_col = format!("{}_id", self.morph_name);
        let type_col = format!("{}_type", self.morph_name);

        // SELECT current pivot rows: keyed by the parent's id + type.
        let (id_ph, type_ph) = match backend {
            DatabaseBackend::Postgres => ("$1".to_string(), "$2".to_string()),
            _ => ("?".to_string(), "?".to_string()),
        };
        let select_sql = format!(
            "SELECT {related_key} AS __sn_related FROM {table} \
              WHERE {id_col} = {id_ph} AND {type_col} = {type_ph}",
            related_key = self.pivot_related_key,
            table = self.pivot_table,
            id_col = id_col,
            type_col = type_col,
            id_ph = id_ph,
            type_ph = type_ph,
        );
        let select_stmt = Statement::from_sql_and_values(
            backend,
            &select_sql,
            vec![
                json_value_to_sea_value(&self.parent_key_value),
                sea_orm::Value::from(self.parent_morph_type.clone()),
            ],
        );
        let rows = exec
            .query_all(select_stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        let mut current_map: HashMap<String, serde_json::Value> = HashMap::new();
        for r in rows.iter() {
            if let Ok(n) = r.try_get::<i64>("", "__sn_related") {
                let v = serde_json::Value::from(n);
                current_map.insert(v.to_string(), v);
            } else if let Ok(s) = r.try_get::<String>("", "__sn_related") {
                let v = serde_json::Value::from(s);
                current_map.insert(v.to_string(), v);
            }
        }
        let current_keys: HashSet<String> = current_map.keys().cloned().collect();

        let target_keys: HashSet<String> =
            target_ids.iter().map(|v| v.to_string()).collect();

        let mut attach_set: Vec<serde_json::Value> = Vec::new();
        for v in target_ids.into_iter() {
            if !current_keys.contains(&v.to_string()) {
                attach_set.push(v);
            }
        }
        let detach_set: Vec<serde_json::Value> = current_map
            .into_iter()
            .filter_map(|(k, v)| (!target_keys.contains(&k)).then_some(v))
            .collect();

        // Atomicity: inherit from CURRENT_TX when active, else open
        // inner SeaORM tx — same precedence as BelongsToMany::sync.
        match &exec {
            ExecutorChoice::Tx(t) => {
                for related_id in detach_set.iter() {
                    morph_detach_one(
                        t.as_ref(),
                        backend,
                        &self.pivot_table,
                        &self.morph_name,
                        &self.pivot_related_key,
                        &self.parent_key_value,
                        &self.parent_morph_type,
                        related_id,
                    )
                    .await?;
                }
                for related_id in attach_set.iter() {
                    morph_attach_one(
                        t.as_ref(),
                        backend,
                        &self.pivot_table,
                        &self.morph_name,
                        &self.pivot_related_key,
                        &self.parent_key_value,
                        &self.parent_morph_type,
                        related_id,
                        Attrs::new(),
                        self.with_timestamps,
                    )
                    .await?;
                }
            }
            ExecutorChoice::Pool(c) => {
                let txn = c
                    .inner()
                    .begin()
                    .await
                    .map_err(|e| FrameworkError::database(e.to_string()))?;
                for related_id in detach_set.iter() {
                    morph_detach_one(
                        &txn,
                        backend,
                        &self.pivot_table,
                        &self.morph_name,
                        &self.pivot_related_key,
                        &self.parent_key_value,
                        &self.parent_morph_type,
                        related_id,
                    )
                    .await?;
                }
                for related_id in attach_set.iter() {
                    morph_attach_one(
                        &txn,
                        backend,
                        &self.pivot_table,
                        &self.morph_name,
                        &self.pivot_related_key,
                        &self.parent_key_value,
                        &self.parent_morph_type,
                        related_id,
                        Attrs::new(),
                        self.with_timestamps,
                    )
                    .await?;
                }
                txn.commit()
                    .await
                    .map_err(|e| FrameworkError::database(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Fetch every related row currently attached to this parent
    /// through the polymorphic pivot. Each row carries its pivot
    /// context via `__pivot` (accessible through the macro-emitted
    /// `.pivot::<P>()` accessor).
    ///
    /// Two-query strategy: fetch related rows by IN-set on the pivot's
    /// related-FK values (filtered by the parent's id + type), then
    /// fetch pivot rows separately and zip via `(parent_id, related_id)`.
    pub async fn get(self) -> Result<Collection<R>, FrameworkError> {
        // Phase 10C audit-fix AF2 — route the pivot-id SELECT through
        // ExecutorChoice so it honors CURRENT_TX. Downstream
        // Model::query() calls already do so via Builder::get.
        let exec = ExecutorChoice::resolve_read(None, None, None).await?;
        let backend = exec.backend();

        let id_col = format!("{}_id", self.morph_name);
        let type_col = format!("{}_type", self.morph_name);

        // Fetch the set of related IDs attached to this parent.
        let (id_ph, type_ph) = match backend {
            DatabaseBackend::Postgres => ("$1".to_string(), "$2".to_string()),
            _ => ("?".to_string(), "?".to_string()),
        };
        let id_sql = format!(
            "SELECT {rk} AS __sn_related FROM {table} \
              WHERE {id_col} = {id_ph} AND {type_col} = {type_ph}",
            rk = self.pivot_related_key,
            table = self.pivot_table,
            id_col = id_col,
            type_col = type_col,
            id_ph = id_ph,
            type_ph = type_ph,
        );
        let id_stmt = Statement::from_sql_and_values(
            backend,
            &id_sql,
            vec![
                json_value_to_sea_value(&self.parent_key_value),
                sea_orm::Value::from(self.parent_morph_type.clone()),
            ],
        );
        let id_rows = exec
            .query_all(id_stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let mut related_ids: Vec<serde_json::Value> = Vec::with_capacity(id_rows.len());
        for r in id_rows.iter() {
            if let Ok(n) = r.try_get::<i64>("", "__sn_related") {
                related_ids.push(serde_json::Value::from(n));
            } else if let Ok(s) = r.try_get::<String>("", "__sn_related") {
                related_ids.push(serde_json::Value::from(s));
            }
        }
        if related_ids.is_empty() {
            return Ok(Collection::new());
        }

        // Fetch the related rows by IN-set on their PK column. The
        // optional `scope_rewrite` closure (set by `with_trashed` /
        // `only_trashed` when `R: SoftDeletes`) widens or restricts
        // the scope on `R`. Non-soft-delete `R`s have no closure to
        // apply.
        let related_rows: Vec<R> = {
            let mut q = R::query().filter_in(self.related_key.as_str(), related_ids.clone());
            if let Some(rw) = self.scope_rewrite {
                q = rw(q);
            }
            q.get().await?.into_vec()
        };

        // Fetch the pivot rows attached to this parent (filtered by
        // both id and type).
        let pivot_rows: Vec<P> = P::query()
            .filter(id_col.as_str(), self.parent_key_value.clone())
            .filter(
                type_col.as_str(),
                serde_json::Value::String(self.parent_morph_type.clone()),
            )
            .get()
            .await?
            .into_vec();

        // Index pivots by related_key value (JSON-string form).
        use std::collections::HashMap;
        let mut by_related: HashMap<String, P> = HashMap::new();
        for p in pivot_rows.into_iter() {
            let p_json = serde_json::to_value(&p).unwrap_or(serde_json::Value::Null);
            let key = p_json
                .get(&self.pivot_related_key)
                .map(|v| v.to_string())
                .unwrap_or_default();
            by_related.insert(key, p);
        }

        // Stamp the pivot context onto each related row via the
        // `EagerLoadDispatch::set_pivot_arc` hook.
        let mut out: Vec<R> = Vec::with_capacity(related_rows.len());
        for r in related_rows.into_iter() {
            let r_json = serde_json::to_value(&r).unwrap_or(serde_json::Value::Null);
            let key = r_json
                .get(&self.related_key)
                .map(|v| v.to_string())
                .unwrap_or_default();
            let mut row = r;
            if let Some(pivot) = by_related.get(&key) {
                row.set_pivot_arc(Some(Arc::new(pivot.clone())));
            }
            out.push(row);
        }
        Ok(Collection::from_vec(out))
    }

    /// Convenience over `get()` — drop everything after the first
    /// related row.
    pub async fn first(self) -> Result<Option<R>, FrameworkError> {
        Ok(self.get().await?.into_vec().into_iter().next())
    }

    /// `SELECT COUNT(*) FROM pivot WHERE <name>_id = ? AND <name>_type = ?`.
    /// Returns `i64` to match [`BelongsToMany::count`](super::BelongsToMany::count).
    pub async fn count(self) -> Result<i64, FrameworkError> {
        // Phase 10C audit-fix AF2 — see attach_with above.
        let exec = ExecutorChoice::resolve_read(None, None, None).await?;
        let backend = exec.backend();
        let (id_ph, type_ph) = match backend {
            DatabaseBackend::Postgres => ("$1".to_string(), "$2".to_string()),
            _ => ("?".to_string(), "?".to_string()),
        };
        let id_col = format!("{}_id", self.morph_name);
        let type_col = format!("{}_type", self.morph_name);
        let sql = format!(
            "SELECT COUNT(*) AS __sn_count FROM {table} \
              WHERE {id_col} = {id_ph} AND {type_col} = {type_ph}",
            table = self.pivot_table,
            id_col = id_col,
            type_col = type_col,
            id_ph = id_ph,
            type_ph = type_ph,
        );
        let stmt = Statement::from_sql_and_values(
            backend,
            &sql,
            vec![
                json_value_to_sea_value(&self.parent_key_value),
                sea_orm::Value::from(self.parent_morph_type),
            ],
        );
        let row = exec
            .query_one(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(row
            .and_then(|r| r.try_get::<i64>("", "__sn_count").ok())
            .unwrap_or(0))
    }
}

/// Soft-delete scope modifiers for `MorphToMany<L, R, P>` when the
/// related (`R`) side is soft-deletable. Same shape as
/// [`BelongsToMany`](super::BelongsToMany)'s equivalent block — the
/// pivot table itself is never filtered for `deleted_at` (pivots are
/// a join artefact), only the related rows. The closure captures the
/// `R: SoftDeletes` bound at construction so [`Self::get`] can call
/// it generic over plain `R: Model`.
impl<L, R, P> MorphToMany<L, R, P>
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
    P: Model + 'static,
    P: From<<P::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <P::Entity as sea_orm::EntityTrait>::Model: From<P>
        + sea_orm::IntoActiveModel<<P::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <P::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<P::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Widen the related-row lookup to include trashed `R` rows.
    pub fn with_trashed(mut self) -> Self {
        self.scope_rewrite = Some(Box::new(|b: Builder<R>| b.with_trashed()));
        self
    }

    /// Restrict the related-row lookup to *only* trashed `R` rows.
    pub fn only_trashed(mut self) -> Self {
        self.scope_rewrite = Some(Box::new(|b: Builder<R>| b.only_trashed()));
        self
    }
}

impl<L, R, P> Relation for MorphToMany<L, R, P>
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
    P: Model,
    P: From<<P::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <P::Entity as sea_orm::EntityTrait>::Model: From<P>
        + sea_orm::IntoActiveModel<<P::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <P::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<P::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    type Parent = L;
    type Target = R;
    const KIND: RelationKind = RelationKind::MorphToMany;

    fn parent_key(&self) -> &str {
        &self.parent_key
    }

    fn foreign_key(&self) -> &str {
        // Surface the morph-name root — the actual pivot columns are
        // `<morph_name>_id` (FK) and `<morph_name>_type`
        // (discriminator). Admin tooling reading the
        // [`RelationEntry`](super::RelationEntry) surfaces this.
        &self.morph_name
    }
}

// ---- MorphedByMany ------------------------------------------------------

/// Inverse polymorphic m2m — from the m2m side `L` (e.g. `Tag`) to one
/// specific morph target family `R` (e.g. `Post` or `Video`) through
/// polymorphic pivot `P`. Constructed by the macro-emitted relation
/// method; user code never calls [`MorphedByMany::__new`] directly.
///
/// Each declaration filters one target morph family — so `Tag.posts()`
/// returns only Post-typed taggables and `Tag.videos()` returns only
/// Video-typed taggables. The target's morph-type string is declared
/// explicitly on the relation via `target_morph_type = "..."` because
/// the macro at L's expansion site can't introspect R's `morph_type`
/// attribute (it lives in a separate `#[suprnova::model]` invocation).
pub struct MorphedByMany<L, R, P>
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
    P: Model + 'static,
    P: From<<P::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <P::Entity as sea_orm::EntityTrait>::Model: From<P>
        + sea_orm::IntoActiveModel<<P::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <P::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<P::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// The m2m side row's key value, JSON-encoded.
    tag_key_value: serde_json::Value,
    /// R's `morph_type` value — the string the pivot's
    /// `<morph_name>_type` column has to equal for the pivot row to
    /// point at this morph target family. Declared via the relation's
    /// `target_morph_type = "..."` option.
    target_morph_type: String,
    /// Morph family name — controls the `<morph_name>_id` /
    /// `<morph_name>_type` column names on the pivot.
    morph_name: String,
    /// Pivot table name.
    pivot_table: String,
    /// Pivot column pointing at the m2m side (`L=Tag`). Default
    /// `<snake(L)>_id`.
    pivot_foreign_key: String,
    /// Related-side primary-key COLUMN name used by the JOIN.
    /// Default `"id"`.
    related_key: String,
    /// Tag-side key column. Default `"id"`. Honoured by the
    /// [`Relation`] impl.
    parent_key: String,
    /// Deferred soft-delete scope rewrite applied to the related-row
    /// query at [`Self::get`] time. See [`MorphToMany::scope_rewrite`]
    /// for the matching closure-erasure pattern.
    scope_rewrite: Option<ScopeRewrite<R>>,
    /// `PhantomData` carries `L`, `R`, `P` so the [`Relation`] impl
    /// can name `type Parent = L` / `type Target = R`.
    #[allow(clippy::type_complexity)]
    _phantom: PhantomData<fn() -> (L, R, P)>,
}

impl<L, R, P> MorphedByMany<L, R, P>
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
    P: Model + 'static,
    P: From<<P::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <P::Entity as sea_orm::EntityTrait>::Model: From<P>
        + sea_orm::IntoActiveModel<<P::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <P::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<P::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Construct a `MorphedByMany`. Invoked by the macro-emitted
    /// relation method.
    #[doc(hidden)]
    pub fn __new(
        tag_key_value: serde_json::Value,
        target_morph_type: String,
        morph_name: String,
        pivot_table: String,
        pivot_foreign_key: String,
    ) -> Self {
        Self {
            tag_key_value,
            target_morph_type,
            morph_name,
            pivot_table,
            pivot_foreign_key,
            related_key: "id".into(),
            parent_key: "id".into(),
            scope_rewrite: None,
            _phantom: PhantomData,
        }
    }

    /// Override the related-side primary-key column name used by the
    /// JOIN in [`Self::get`]. Default `"id"`.
    pub fn related_pk(mut self, key: impl Into<String>) -> Self {
        self.related_key = key.into();
        self
    }

    /// Override the tag-side (parent's) key column. Default `"id"`.
    pub fn local_key(mut self, key: impl Into<String>) -> Self {
        self.parent_key = key.into();
        self
    }

    /// Fetch every R row attached to this Tag via the polymorphic
    /// pivot, filtered to the declared target morph family. Each
    /// returned R carries its pivot context via `__pivot` (accessible
    /// through the macro-emitted `.pivot::<P>()` accessor), matching
    /// the symmetric `MorphToMany::get()` contract.
    ///
    /// Two-query strategy. Query 1: SELECT pivot rows where the
    /// tag-side FK matches this Tag and the `<morph_name>_type` column
    /// matches the declared target morph type. Query 2: SELECT R rows
    /// by IN-set on those `<morph_name>_id` values. Zip via the pivot's
    /// `<morph_name>_id` column to stamp `__pivot` per R.
    pub async fn get(self) -> Result<Collection<R>, FrameworkError> {
        let id_col = format!("{}_id", self.morph_name);
        let type_col = format!("{}_type", self.morph_name);

        // Query 1: pivot rows, full row (we need the morph-id column
        // to zip + the rest for `__pivot` context). Goes through the
        // typed `Builder<P>` path so casts on P's columns flow through
        // SeaORM's deserialiser correctly.
        let pivot_rows: Vec<P> = P::query()
            .filter(
                self.pivot_foreign_key.as_str(),
                self.tag_key_value.clone(),
            )
            .filter(
                type_col.as_str(),
                serde_json::Value::String(self.target_morph_type.clone()),
            )
            .get()
            .await?
            .into_vec();
        if pivot_rows.is_empty() {
            return Ok(Collection::new());
        }

        // Pull morph-target IDs out of the pivot rows.
        let mut target_ids: Vec<serde_json::Value> = Vec::with_capacity(pivot_rows.len());
        let mut seen_target: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for pv in pivot_rows.iter() {
            let pj = serde_json::to_value(pv).unwrap_or(serde_json::Value::Null);
            if let Some(v) = pj.get(&id_col) {
                let s = v.to_string();
                if seen_target.insert(s) {
                    target_ids.push(v.clone());
                }
            }
        }

        // Query 2: target rows by PK IN-set. The scope_rewrite hook
        // (set by `with_trashed` / `only_trashed` for soft-delete R)
        // runs against the inner builder before `.get()`.
        let target_rows: Vec<R> = {
            let mut q = R::query().filter_in(self.related_key.as_str(), target_ids);
            if let Some(rw) = self.scope_rewrite {
                q = rw(q);
            }
            q.get().await?.into_vec()
        };

        // Index pivots by `<morph_name>_id` (JSON-string form).
        use std::collections::HashMap;
        let mut by_target: HashMap<String, P> = HashMap::new();
        for pv in pivot_rows.into_iter() {
            let pj = serde_json::to_value(&pv).unwrap_or(serde_json::Value::Null);
            let key = pj
                .get(&id_col)
                .map(|v| v.to_string())
                .unwrap_or_default();
            by_target.insert(key, pv);
        }

        // Stamp the pivot context onto each target row via the
        // `EagerLoadDispatch::set_pivot_arc` hook.
        let mut out: Vec<R> = Vec::with_capacity(target_rows.len());
        for r in target_rows.into_iter() {
            let r_json = serde_json::to_value(&r).unwrap_or(serde_json::Value::Null);
            let key = r_json
                .get(&self.related_key)
                .map(|v| v.to_string())
                .unwrap_or_default();
            let mut row = r;
            if let Some(pivot) = by_target.get(&key) {
                row.set_pivot_arc(Some(Arc::new(pivot.clone())));
            }
            out.push(row);
        }
        Ok(Collection::from_vec(out))
    }

    /// Convenience over `get()` — drop everything after the first row.
    pub async fn first(self) -> Result<Option<R>, FrameworkError> {
        Ok(self.get().await?.into_vec().into_iter().next())
    }

    /// `SELECT COUNT(*) FROM pivot WHERE pfk = ? AND <name>_type = ?`.
    pub async fn count(self) -> Result<i64, FrameworkError> {
        // Phase 10C audit-fix AF2 — route the count through
        // ExecutorChoice so it honors CURRENT_TX.
        let exec = ExecutorChoice::resolve_read(None, None, None).await?;
        let backend = exec.backend();
        let (id_ph, type_ph) = match backend {
            DatabaseBackend::Postgres => ("$1".to_string(), "$2".to_string()),
            _ => ("?".to_string(), "?".to_string()),
        };
        let type_col = format!("{}_type", self.morph_name);
        let sql = format!(
            "SELECT COUNT(*) AS __sn_count FROM {table} \
              WHERE {pfk} = {id_ph} AND {type_col} = {type_ph}",
            table = self.pivot_table,
            pfk = self.pivot_foreign_key,
            type_col = type_col,
            id_ph = id_ph,
            type_ph = type_ph,
        );
        let stmt = Statement::from_sql_and_values(
            backend,
            &sql,
            vec![
                json_value_to_sea_value(&self.tag_key_value),
                sea_orm::Value::from(self.target_morph_type),
            ],
        );
        let row = exec
            .query_one(stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(row
            .and_then(|r| r.try_get::<i64>("", "__sn_count").ok())
            .unwrap_or(0))
    }
}

/// Soft-delete scope modifiers for `MorphedByMany<L, R, P>` when the
/// related (`R`) side is soft-deletable. The pivot table itself is
/// never filtered for `deleted_at` (pivots are a join artefact);
/// only the related-row IN-set query gets the rewrite. Same
/// closure-erasure pattern as the
/// [`MorphToMany`] block above.
impl<L, R, P> MorphedByMany<L, R, P>
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
    P: Model + 'static,
    P: From<<P::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <P::Entity as sea_orm::EntityTrait>::Model: From<P>
        + sea_orm::IntoActiveModel<<P::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <P::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<P::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Widen the related-row lookup to include trashed `R` rows.
    pub fn with_trashed(mut self) -> Self {
        self.scope_rewrite = Some(Box::new(|b: Builder<R>| b.with_trashed()));
        self
    }

    /// Restrict the related-row lookup to *only* trashed `R` rows.
    pub fn only_trashed(mut self) -> Self {
        self.scope_rewrite = Some(Box::new(|b: Builder<R>| b.only_trashed()));
        self
    }
}

impl<L, R, P> Relation for MorphedByMany<L, R, P>
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
    P: Model,
    P: From<<P::Entity as sea_orm::EntityTrait>::Model>
        + serde::Serialize
        + serde::de::DeserializeOwned
        + crate::eloquent::EagerLoadDispatch,
    <P::Entity as sea_orm::EntityTrait>::Model: From<P>
        + sea_orm::IntoActiveModel<<P::Entity as sea_orm::EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + serde::Serialize
        + Send
        + Sync,
    <P::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<P::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    type Parent = L;
    type Target = R;
    const KIND: RelationKind = RelationKind::MorphedByMany;

    fn parent_key(&self) -> &str {
        &self.parent_key
    }

    fn foreign_key(&self) -> &str {
        &self.pivot_foreign_key
    }
}

// ---- Internal helpers ----------------------------------------------------

/// INSERT path shared by `attach` / `attach_with` / `sync`. The
/// `morph_name` controls the `<morph_name>_id` and `<morph_name>_type`
/// column names; `parent_morph_type` is the string written to the
/// type column. Same connection-or-transaction abstraction as
/// BelongsToMany.
#[allow(clippy::too_many_arguments)]
async fn morph_attach_one<C: ConnectionTrait>(
    conn: &C,
    backend: DatabaseBackend,
    pivot_table: &str,
    morph_name: &str,
    pivot_related_key: &str,
    parent_id: &serde_json::Value,
    parent_morph_type: &str,
    related_id: &serde_json::Value,
    extra: Attrs,
    with_timestamps: bool,
) -> Result<(), FrameworkError> {
    let id_col = format!("{morph_name}_id");
    let type_col = format!("{morph_name}_type");

    let mut columns: Vec<String> = vec![
        pivot_related_key.to_string(),
        id_col.clone(),
        type_col.clone(),
    ];
    let mut values: Vec<sea_orm::Value> = vec![
        json_value_to_sea_value(related_id),
        json_value_to_sea_value(parent_id),
        sea_orm::Value::from(parent_morph_type.to_string()),
    ];
    for (k, v) in extra.iter() {
        if k == pivot_related_key || k == id_col.as_str() || k == type_col.as_str() {
            continue;
        }
        columns.push(k.to_string());
        values.push(json_value_to_sea_value(v));
    }
    if with_timestamps {
        let now = chrono::Utc::now().to_rfc3339();
        if !columns.iter().any(|c| c == "created_at") {
            columns.push("created_at".to_string());
            values.push(sea_orm::Value::from(now.clone()));
        }
        if !columns.iter().any(|c| c == "updated_at") {
            columns.push("updated_at".to_string());
            values.push(sea_orm::Value::from(now));
        }
    }

    let placeholders: Vec<String> = (1..=columns.len())
        .map(|i| match backend {
            DatabaseBackend::Postgres => format!("${i}"),
            _ => "?".to_string(),
        })
        .collect();

    let sql = format!(
        "INSERT INTO {table} ({cols}) VALUES ({phs})",
        table = pivot_table,
        cols = columns.join(", "),
        phs = placeholders.join(", "),
    );
    let stmt = Statement::from_sql_and_values(backend, &sql, values);
    conn.execute(stmt)
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;
    Ok(())
}

/// DELETE path shared by `detach` / `sync`. Filters by all three of
/// `pivot_related_key = related_id`, `<morph_name>_id = parent_id`,
/// and `<morph_name>_type = parent_morph_type`.
#[allow(clippy::too_many_arguments)]
async fn morph_detach_one<C: ConnectionTrait>(
    conn: &C,
    backend: DatabaseBackend,
    pivot_table: &str,
    morph_name: &str,
    pivot_related_key: &str,
    parent_id: &serde_json::Value,
    parent_morph_type: &str,
    related_id: &serde_json::Value,
) -> Result<(), FrameworkError> {
    let id_col = format!("{morph_name}_id");
    let type_col = format!("{morph_name}_type");
    let (ph1, ph2, ph3) = match backend {
        DatabaseBackend::Postgres => (
            "$1".to_string(),
            "$2".to_string(),
            "$3".to_string(),
        ),
        _ => ("?".to_string(), "?".to_string(), "?".to_string()),
    };
    let sql = format!(
        "DELETE FROM {table} WHERE {rk} = {ph1} AND {id_col} = {ph2} AND {type_col} = {ph3}",
        table = pivot_table,
        rk = pivot_related_key,
        id_col = id_col,
        type_col = type_col,
        ph1 = ph1,
        ph2 = ph2,
        ph3 = ph3,
    );
    let stmt = Statement::from_sql_and_values(
        backend,
        &sql,
        vec![
            json_value_to_sea_value(related_id),
            json_value_to_sea_value(parent_id),
            sea_orm::Value::from(parent_morph_type.to_string()),
        ],
    );
    conn.execute(stmt)
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;
    Ok(())
}
