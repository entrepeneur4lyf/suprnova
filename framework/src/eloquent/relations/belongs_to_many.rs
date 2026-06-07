//! `BelongsToMany` — many-to-many through a first-class Pivot model.
//!
//! Mirrors Laravel's
//! [`belongsToMany`](https://laravel.com/docs/12.x/eloquent-relationships#many-to-many)
//! semantics: a join (pivot) table carries one FK to each side. The
//! pivot in Suprnova is itself a `#[suprnova::model]` struct — own
//! migrations, own accessors, own events. Extra columns and timestamps
//! are surfaced on the loaded related rows via `__pivot`, accessed
//! through the macro-emitted `r.pivot::<RoleUserPivot>()` accessor.
//!
//! Default key conventions:
//!
//! - `pivot_foreign_key` (pivot column → L): `<snake(parent_struct)>_id`
//! - `pivot_related_key` (pivot column → R): `<snake(target_struct)>_id`
//! - `pivot_table`: `<P as EloquentModel>::TABLE`
//! - `parent_key` / `related_key`: `"id"`
//!
//! All four customisable via the macro's `pivot_foreign_key = "..."` /
//! `pivot_related_key = "..."` / `pivot_table = "..."` / `lk = "..."`
//! options.
//!
//! Mutators:
//!
//! - [`attach`](BelongsToMany::attach) — INSERT a single pivot row.
//! - [`attach_with`](BelongsToMany::attach_with) — INSERT with extra pivot
//!   columns (and timestamps if `with_timestamps()` is set).
//! - [`detach`](BelongsToMany::detach) — DELETE a single pivot row.
//! - [`sync`](BelongsToMany::sync) — diff-and-apply against the current pivot
//!   set; runs attach + detach inside a `DatabaseTransaction` so a
//!   partial failure rolls back.
//!
//! Readers:
//!
//! - [`get`](BelongsToMany::get) — two-query strategy: fetch related rows via
//!   `JOIN`, fetch pivot rows separately, zip via `(parent_id,
//!   related_id)`, stamp `__pivot` on each clone.
//! - [`first`](BelongsToMany::first) — `.get().into_iter().next()`.
//! - [`count`](BelongsToMany::count) — `SELECT COUNT(*) FROM pivot WHERE
//!   pivot_foreign_key = ?`.
//!
//! Eager loading happens through the parent model's `__eager_load`
//! match arm — emitted by `#[suprnova::model]` and exercised by
//! `User::with(["roles"])`. The arm clones each loaded R per attached
//! parent so multiple parents sharing one R each get their own copy
//! of the pivot context.

use std::marker::PhantomData;
use std::sync::Arc;

use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TransactionTrait};

use crate::database::transaction::ExecutorChoice;
use crate::eloquent::EloquentModel;
use crate::eloquent::attrs::Attrs;
use crate::eloquent::builder::Builder;
use crate::eloquent::collection::Collection;
use crate::eloquent::model::{Model, json_value_to_sea_value};
use crate::eloquent::relations::{Relation, RelationKind};
use crate::error::FrameworkError;

/// Boxed builder-rewrite closure for [`BelongsToMany::with_trashed`] /
/// [`BelongsToMany::only_trashed`]. Aliased so the field declaration
/// satisfies clippy's `type_complexity` lint and reads as one type.
/// Same shape as [`super::belongs_to::ScopeRewrite`][crate::eloquent::relations::belongs_to]
/// — the soft-delete bound is captured at closure construction time.
type ScopeRewrite<R> = Box<dyn FnOnce(Builder<R>) -> Builder<R> + Send>;

/// Many-to-many relation from parent `L` to related `R` through pivot
/// `P`. Constructed by the macro-emitted relation method
/// (`fn roles(&self) -> BelongsToMany<Self, Role, RoleUserPivot>`);
/// user code never calls [`BelongsToMany::__new`] directly.
///
/// The wrapper holds the FK / key / column metadata plus the parent's
/// PK value, all paid up at construction time. Terminal methods
/// (`attach`, `detach`, `sync`, `get`, `first`, `count`) issue the
/// SQL.
pub struct BelongsToMany<L, R, P>
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
    /// Parent row's local-key value, JSON-encoded. The macro emits
    /// `serde_json::to_value(&self.id)` at the call site so the
    /// runtime path stays homogeneous regardless of the PK type
    /// (`i64`, `String`, `Uuid`-via-string, ...).
    parent_key_value: serde_json::Value,
    /// Pivot table name. Defaults to `<P as EloquentModel>::TABLE` —
    /// the pivot's own `#[suprnova::model(table = "...")]` declaration
    /// is the single source of truth. Override via the macro's
    /// `pivot_table = "..."` option.
    pivot_table: String,
    /// Pivot column pointing at the parent (`L`). Default:
    /// `<snake(parent_struct)>_id`. Override via `pivot_foreign_key`.
    pivot_foreign_key: String,
    /// Pivot column pointing at the related (`R`). Default:
    /// `<snake(target_struct)>_id`. Override via `pivot_related_key`.
    pivot_related_key: String,
    /// Parent table's key column. Default `"id"`. Honoured by the
    /// [`Relation`] impl.
    parent_key: String,
    /// Related table's key column. Default `"id"`. Used by the JOIN
    /// in [`Self::get`].
    related_key: String,
    /// Extra pivot columns to project into `__pivot`. Always includes
    /// the two FK columns implicitly — `pivot_columns` is for the
    /// "extras" (`assigned_at`, `notes`, custom data).
    pivot_columns: Vec<String>,
    /// When true, the attach path stamps `created_at` / `updated_at`
    /// on the pivot row and the loader surfaces both columns in the
    /// pivot context.
    with_timestamps: bool,
    /// Deferred soft-delete scope rewrite applied to the related-row
    /// query at [`Self::get`] / [`Self::first`] time. Only ever set
    /// by [`Self::with_trashed`] / [`Self::only_trashed`], both gated
    /// on `R: SoftDeletes`. See
    /// [`BelongsTo::scope_rewrite`](super::belongs_to::BelongsTo) for
    /// the matching closure-erasure pattern.
    scope_rewrite: Option<ScopeRewrite<R>>,
    /// PhantomData carries `L`, `R`, `P` so the [`Relation`] impl can
    /// name `type Parent = L` / `type Target = R` without runtime
    /// fields. `fn() -> (L, R, P)` keeps the type covariant +
    /// `Send + Sync` regardless of the parameters.
    #[allow(clippy::type_complexity)]
    _phantom: PhantomData<fn() -> (L, R, P)>,
}

impl<L, R, P> BelongsToMany<L, R, P>
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
    // `P: 'static` is required because `set_pivot_arc` stores the
    // pivot row inside `Arc<dyn Any + Send + Sync>`, which has a
    // `'static` lifetime bound. Every `#[suprnova::model]`-generated
    // struct is `'static` in practice (no borrowed fields), so this
    // is purely a where-clause witness.
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
    /// Construct a `BelongsToMany`. Invoked by the macro-emitted
    /// relation method.
    #[doc(hidden)]
    pub fn __new(
        parent_key_value: serde_json::Value,
        pivot_table: String,
        pivot_foreign_key: String,
        pivot_related_key: String,
    ) -> Self {
        Self {
            parent_key_value,
            pivot_table,
            pivot_foreign_key,
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
    /// The two FK columns (`pivot_foreign_key`, `pivot_related_key`)
    /// are always loaded; this option is for "extras" — `assigned_at`,
    /// `notes`, custom payloads.
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
    /// by `attach` / `attach_with` / `sync`, and surface both columns
    /// in the loaded pivot context. Mirrors Laravel's
    /// `->withTimestamps()`.
    pub fn with_timestamps(mut self) -> Self {
        self.with_timestamps = true;
        self
    }

    /// Override the pivot column pointing at the parent.
    pub fn foreign_key(mut self, key: impl Into<String>) -> Self {
        self.pivot_foreign_key = key.into();
        self
    }

    /// Override the pivot column pointing at the related row.
    pub fn related_key(mut self, key: impl Into<String>) -> Self {
        self.pivot_related_key = key.into();
        self
    }

    /// Override the parent's key column. Only updates the metadata
    /// surface; the runtime parent value was extracted at construction.
    pub fn local_key(mut self, key: impl Into<String>) -> Self {
        self.parent_key = key.into();
        self
    }

    /// Override the related-side primary-key COLUMN name used by
    /// [`Self::get`]'s IN-set filter and the macro-emitted aggregate
    /// JOIN (`__sn_r.<col> = __sn_p.<pivot_related_key>`). Defaults to
    /// `"id"`. Set this when the related model declares a non-`id`
    /// primary key via `#[model(primary_key = "uuid")]` (or similar) —
    /// without it, `.get()` filters on the wrong column and the
    /// aggregate JOIN errors with "no such column: __sn_r.id".
    ///
    /// Named `related_pk` (not `related_key`) so it doesn't collide
    /// with the existing [`Self::related_key`] builder, which sets the
    /// pivot-side related FK column.
    pub fn related_pk(mut self, key: impl Into<String>) -> Self {
        self.related_key = key.into();
        self
    }

    /// Insert a pivot row linking the parent to `related_id`.
    /// Equivalent to `attach_with(related_id, Attrs::new())`.
    ///
    /// Mirrors Laravel's `->attach($id)`. Idempotency is not
    /// guaranteed — if the pivot has a UNIQUE constraint on
    /// `(parent_id, related_id)`, a second `attach()` of the same
    /// pair returns a database error. Use [`Self::sync`] to set a
    /// full set without duplicate INSERTs.
    pub async fn attach(
        self,
        related_id: impl Into<serde_json::Value>,
    ) -> Result<(), FrameworkError> {
        self.attach_with(related_id, Attrs::new()).await
    }

    /// Insert a pivot row with extra column values (and timestamps
    /// when `with_timestamps()` is on). Mirrors Laravel's
    /// `->attach($id, ['note' => '...'])`.
    ///
    /// # Security
    ///
    /// Keys of `extra` are pivot column names — they interpolate
    /// **raw** into the rendered `INSERT INTO pivot (...) VALUES (...)`
    /// SQL (same SQL-identifier contract as
    /// [`Builder::filter`](crate::eloquent::Builder::filter) — see the
    /// builder module docs). The Fillable / Guarded mass-assignment
    /// guard does NOT apply at this layer. **Never accept the key
    /// names from untrusted input**; hardcode them via the
    /// [`attrs!`](crate::attrs) macro (which stringifies identifier
    /// keys at compile time) or pick from a known allowlist. The
    /// values are parameterised binds and ARE safe to take from
    /// untrusted input.
    pub async fn attach_with(
        self,
        related_id: impl Into<serde_json::Value>,
        extra: Attrs,
    ) -> Result<(), FrameworkError> {
        // Phase 10C audit-fix AF2 — resolve through ExecutorChoice so the
        // pivot INSERT lands on the ambient transaction connection when
        // CURRENT_TX is active. The pre-fix path called DB::connection()
        // directly and silently auto-committed on the pool.
        let exec = ExecutorChoice::resolve_write(None, None, None).await?;
        let backend = exec.backend();
        let id = related_id.into();
        match &exec {
            ExecutorChoice::Tx(t, _) => {
                attach_one(
                    t.as_ref(),
                    backend,
                    &self.pivot_table,
                    &self.pivot_foreign_key,
                    &self.pivot_related_key,
                    &self.parent_key_value,
                    &id,
                    extra,
                    self.with_timestamps,
                )
                .await
            }
            ExecutorChoice::Pool(c, _) => {
                attach_one(
                    c.inner(),
                    backend,
                    &self.pivot_table,
                    &self.pivot_foreign_key,
                    &self.pivot_related_key,
                    &self.parent_key_value,
                    &id,
                    extra,
                    self.with_timestamps,
                )
                .await
            }
        }
    }

    /// Delete pivot rows linking the parent to `related_id`. Mirrors
    /// Laravel's `->detach($id)`.
    pub async fn detach(
        self,
        related_id: impl Into<serde_json::Value>,
    ) -> Result<(), FrameworkError> {
        // Phase 10C audit-fix AF2 — resolve through ExecutorChoice so the
        // pivot DELETE lands on the ambient transaction connection when
        // CURRENT_TX is active.
        let exec = ExecutorChoice::resolve_write(None, None, None).await?;
        let backend = exec.backend();
        let id = related_id.into();
        match &exec {
            ExecutorChoice::Tx(t, _) => {
                detach_one(
                    t.as_ref(),
                    backend,
                    &self.pivot_table,
                    &self.pivot_foreign_key,
                    &self.pivot_related_key,
                    &self.parent_key_value,
                    &id,
                )
                .await
            }
            ExecutorChoice::Pool(c, _) => {
                detach_one(
                    c.inner(),
                    backend,
                    &self.pivot_table,
                    &self.pivot_foreign_key,
                    &self.pivot_related_key,
                    &self.parent_key_value,
                    &id,
                )
                .await
            }
        }
    }

    /// Replace the parent's full set of attached relations with the
    /// given IDs. Mirrors Laravel's `->sync([...])`.
    ///
    /// 1. SELECT current pivot rows for this parent.
    /// 2. Compute `attach_set = ids - current` and
    ///    `detach_set = current - ids`.
    /// 3. Execute the attaches + detaches inside a single
    ///    `DatabaseTransaction` so a partial failure rolls back.
    ///
    /// IDs in `ids` are normalised by their JSON string form (matching
    /// the framework-wide FK-key-as-string convention), so duplicates
    /// in the input set collapse to one attach.
    pub async fn sync<I, V>(self, ids: I) -> Result<(), FrameworkError>
    where
        I: IntoIterator<Item = V>,
        V: Into<serde_json::Value>,
    {
        use std::collections::{HashMap, HashSet};

        // Phase 10C audit-fix AF2 — resolve through ExecutorChoice so the
        // SELECT + INSERTs + DELETEs all run on the ambient transaction
        // connection when CURRENT_TX is active. Outside a tx we still
        // open an inner SeaORM transaction (below) for atomicity of the
        // attach/detach loop; that inner tx is unnecessary when we
        // already inherit one from the closure form.
        let exec = ExecutorChoice::resolve_write(None, None, None).await?;
        let backend = exec.backend();

        // De-duplicate target IDs by JSON-string canonicalisation.
        // Preserves the first occurrence for a deterministic insert
        // order (relevant for snapshot tests).
        let mut seen_target: HashSet<String> = HashSet::new();
        let mut target_ids: Vec<serde_json::Value> = Vec::new();
        for raw in ids {
            let v: serde_json::Value = raw.into();
            let key = v.to_string();
            if seen_target.insert(key) {
                target_ids.push(v);
            }
        }

        // SELECT current pivot rows: only the related-key column is
        // needed for the diff. Backend-aware placeholder for the
        // single parent-key bind.
        let select_ph = match backend {
            DatabaseBackend::Postgres => "$1".to_string(),
            _ => "?".to_string(),
        };
        let select_sql = format!(
            "SELECT {related_key} AS __sn_related FROM {table} WHERE {fk} = {ph}",
            related_key = self.pivot_related_key,
            table = self.pivot_table,
            fk = self.pivot_foreign_key,
            ph = select_ph,
        );
        let select_stmt = Statement::from_sql_and_values(
            backend,
            &select_sql,
            vec![json_value_to_sea_value(&self.parent_key_value)],
        );
        let rows = exec
            .query_all(select_stmt)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        // Pull each row's related-key as a JSON value so the diff
        // matches by the same shape as the input set.
        let mut current_map: HashMap<String, serde_json::Value> = HashMap::new();
        for r in rows.iter() {
            // The column may come back as i64, String, etc. — try the
            // common shapes; falling back to the textual form covers
            // exotic PKs. The key for the HashMap is always the JSON
            // string form of whatever we recover.
            if let Ok(n) = r.try_get::<i64>("", "__sn_related") {
                let v = serde_json::Value::from(n);
                current_map.insert(v.to_string(), v);
            } else if let Ok(s) = r.try_get::<String>("", "__sn_related") {
                let v = serde_json::Value::from(s);
                current_map.insert(v.to_string(), v);
            }
        }
        let current_keys: HashSet<String> = current_map.keys().cloned().collect();

        let target_keys: HashSet<String> = target_ids.iter().map(|v| v.to_string()).collect();

        // attach_set = target - current
        let mut attach_set: Vec<serde_json::Value> = Vec::new();
        for v in target_ids.into_iter() {
            if !current_keys.contains(&v.to_string()) {
                attach_set.push(v);
            }
        }
        // detach_set = current - target
        let detach_set: Vec<serde_json::Value> = current_map
            .into_iter()
            .filter_map(|(k, v)| (!target_keys.contains(&k)).then_some(v))
            .collect();

        // Transactional attach + detach. Either all rows commit or
        // none do. When we already inherit a tx via `CURRENT_TX` the
        // ambient one provides atomicity — opening a nested SeaORM
        // begin() inside a tx connection would silently degrade to a
        // savepoint that the outer rollback would still discard, so we
        // just write directly via the executor. Outside a tx we still
        // wrap the writes in an inner SeaORM transaction so a partial
        // failure rolls back.
        match &exec {
            ExecutorChoice::Tx(t, _) => {
                for related_id in detach_set.iter() {
                    detach_one(
                        t.as_ref(),
                        backend,
                        &self.pivot_table,
                        &self.pivot_foreign_key,
                        &self.pivot_related_key,
                        &self.parent_key_value,
                        related_id,
                    )
                    .await?;
                }
                for related_id in attach_set.iter() {
                    attach_one(
                        t.as_ref(),
                        backend,
                        &self.pivot_table,
                        &self.pivot_foreign_key,
                        &self.pivot_related_key,
                        &self.parent_key_value,
                        related_id,
                        Attrs::new(),
                        self.with_timestamps,
                    )
                    .await?;
                }
            }
            ExecutorChoice::Pool(c, _) => {
                let txn = c
                    .inner()
                    .begin()
                    .await
                    .map_err(|e| FrameworkError::database(e.to_string()))?;
                for related_id in detach_set.iter() {
                    detach_one(
                        &txn,
                        backend,
                        &self.pivot_table,
                        &self.pivot_foreign_key,
                        &self.pivot_related_key,
                        &self.parent_key_value,
                        related_id,
                    )
                    .await?;
                }
                for related_id in attach_set.iter() {
                    attach_one(
                        &txn,
                        backend,
                        &self.pivot_table,
                        &self.pivot_foreign_key,
                        &self.pivot_related_key,
                        &self.parent_key_value,
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

    /// Fetch every related row currently attached to this parent.
    /// Each row carries its pivot context via `__pivot`, accessible
    /// through the macro-emitted `.pivot::<P>()` accessor.
    ///
    /// Strategy:
    ///
    /// 1. Fetch related rows via `Builder<R>` with
    ///    `WHERE id IN (SELECT related_key FROM pivot WHERE fk = ?)`.
    /// 2. Fetch pivot rows via `Builder<P>` with `WHERE fk = ?`.
    /// 3. Build `HashMap<related_id, P>` from the second query.
    /// 4. Walk the related rows; for each, stamp
    ///    `__pivot = Some(Arc::new(pivot_row))`.
    ///
    /// The two-query split is intentional: a single `SELECT R.*, P.*
    /// FROM ... JOIN ...` would require column-prefix splitting in
    /// SeaORM's deserialisation path, which is not first-class on the
    /// `FromQueryResult` derive. Two homogeneous queries each round-
    /// trip the rows cleanly through each model's own deserialiser.
    pub async fn get(self) -> Result<Collection<R>, FrameworkError> {
        // Phase 10C audit-fix AF2 — the pivot-id SELECT used to read
        // against the pool; route it through ExecutorChoice so it
        // honors CURRENT_TX. The downstream `Model::query()` calls for
        // the related and pivot rows already consult CURRENT_TX
        // through Builder::get's own ExecutorChoice resolution.
        let exec = ExecutorChoice::resolve_read(None, None, None).await?;
        let backend = exec.backend();

        // Fetch the set of related IDs attached to this parent.
        let id_ph = match backend {
            DatabaseBackend::Postgres => "$1".to_string(),
            _ => "?".to_string(),
        };
        let id_sql = format!(
            "SELECT {rk} AS __sn_related FROM {table} WHERE {fk} = {ph}",
            rk = self.pivot_related_key,
            table = self.pivot_table,
            fk = self.pivot_foreign_key,
            ph = id_ph,
        );
        let id_stmt = Statement::from_sql_and_values(
            backend,
            &id_sql,
            vec![json_value_to_sea_value(&self.parent_key_value)],
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

        // Fetch the related rows by IN-set on their PK column. Any
        // pending scope rewrite (set by `with_trashed` / `only_trashed`
        // on a soft-delete `R`) runs against the inner builder
        // before `.get()`. Non-soft-delete `R`s never construct one,
        // so this is a per-call indirection of a single `Option::map`.
        let related_rows: Vec<R> = {
            let mut q = R::query().filter_in(self.related_key.as_str(), related_ids.clone());
            if let Some(rw) = self.scope_rewrite {
                q = rw(q);
            }
            q.get().await?.into_vec()
        };

        // Fetch the pivot rows attached to this parent.
        let pivot_rows: Vec<P> = P::query()
            .filter(
                self.pivot_foreign_key.as_str(),
                self.parent_key_value.clone(),
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
        // `EagerLoadDispatch::set_pivot_arc` hook — the field
        // (`row.__pivot`) isn't reachable from generic code, but
        // every `#[suprnova::model]` struct ships the setter.
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

    /// `SELECT COUNT(*) FROM pivot WHERE pivot_foreign_key = ?`.
    /// Returns `i64` to match the [`crate::eloquent::HasMany::count`]
    /// surface.
    pub async fn count(self) -> Result<i64, FrameworkError> {
        // Phase 10C audit-fix AF2 — read via ExecutorChoice so a count
        // taken inside `DB::transaction { ... }` sees in-tx pivot
        // attaches/detaches.
        let exec = ExecutorChoice::resolve_read(None, None, None).await?;
        let backend = exec.backend();
        let ph = match backend {
            DatabaseBackend::Postgres => "$1".to_string(),
            _ => "?".to_string(),
        };
        let sql = format!(
            "SELECT COUNT(*) AS __sn_count FROM {table} WHERE {fk} = {ph}",
            table = self.pivot_table,
            fk = self.pivot_foreign_key,
            ph = ph,
        );
        let stmt = Statement::from_sql_and_values(
            backend,
            &sql,
            vec![json_value_to_sea_value(&self.parent_key_value)],
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

/// Soft-delete scope modifiers for `BelongsToMany<L, R, P>` when the
/// related (`R`) side is soft-deletable. The pivot table itself is
/// never filtered for `deleted_at` — pivot rows are a join artefact,
/// not a domain object that gets archived. Matches Laravel's
/// `withTrashed()` shape: applies to the related table, not the
/// intermediate. Future tasks can layer a separate
/// `with_trashed_pivot` if a custom pivot model declares its own
/// `soft_deletes`.
impl<L, R, P> BelongsToMany<L, R, P>
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

impl<L, R, P> Relation for BelongsToMany<L, R, P>
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
    const KIND: RelationKind = RelationKind::BelongsToMany;

    fn parent_key(&self) -> &str {
        &self.parent_key
    }

    fn foreign_key(&self) -> &str {
        &self.pivot_foreign_key
    }
}

// ---- Internal helpers ----------------------------------------------------

/// Shared INSERT path used by `attach` / `attach_with` / `sync`. The
/// connection-or-transaction handle is taken as a generic `&C: ConnectionTrait`
/// so the same routine runs against both `DatabaseConnection` and
/// `DatabaseTransaction`.
#[allow(clippy::too_many_arguments)]
async fn attach_one<C: ConnectionTrait>(
    conn: &C,
    backend: DatabaseBackend,
    pivot_table: &str,
    pivot_foreign_key: &str,
    pivot_related_key: &str,
    parent_id: &serde_json::Value,
    related_id: &serde_json::Value,
    extra: Attrs,
    with_timestamps: bool,
) -> Result<(), FrameworkError> {
    // Build the column / value lists deterministically:
    //   FK columns first, then `extra` (skipping the FK columns if the
    //   user passed them, so the caller-provided overrides don't
    //   double-write), then timestamps if enabled.
    let mut columns: Vec<String> =
        vec![pivot_foreign_key.to_string(), pivot_related_key.to_string()];
    let mut values: Vec<sea_orm::Value> = vec![
        json_value_to_sea_value(parent_id),
        json_value_to_sea_value(related_id),
    ];
    for (k, v) in extra.iter() {
        if k == pivot_foreign_key || k == pivot_related_key {
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

    // Backend-aware placeholders.
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

/// Shared DELETE path used by `detach` / `sync`. Same connection
/// abstraction as [`attach_one`].
async fn detach_one<C: ConnectionTrait>(
    conn: &C,
    backend: DatabaseBackend,
    pivot_table: &str,
    pivot_foreign_key: &str,
    pivot_related_key: &str,
    parent_id: &serde_json::Value,
    related_id: &serde_json::Value,
) -> Result<(), FrameworkError> {
    let (ph1, ph2) = match backend {
        DatabaseBackend::Postgres => ("$1".to_string(), "$2".to_string()),
        _ => ("?".to_string(), "?".to_string()),
    };
    let sql = format!(
        "DELETE FROM {table} WHERE {fk} = {ph1} AND {rk} = {ph2}",
        table = pivot_table,
        fk = pivot_foreign_key,
        rk = pivot_related_key,
        ph1 = ph1,
        ph2 = ph2,
    );
    let stmt = Statement::from_sql_and_values(
        backend,
        &sql,
        vec![
            json_value_to_sea_value(parent_id),
            json_value_to_sea_value(related_id),
        ],
    );
    conn.execute(stmt)
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;
    Ok(())
}
