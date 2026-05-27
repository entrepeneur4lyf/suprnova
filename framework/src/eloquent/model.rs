//! Eloquent Model trait — the CRUD lifecycle layer.
//!
//! Implemented by the `#[suprnova::model]` macro on every annotated
//! struct, this trait carries the bulk of the Eloquent API surface:
//! `find` / `find_or_fail` / `find_many` / `all` / `query` /
//! `create` / `save` / `update` / `delete` / `force_delete` /
//! `refresh` / `fresh` / `replicate` / `replicate_except` /
//! `replicate_into` / `increment` / `decrement`. The companion
//! [`FirstOrCreate`] trait carries the first-or-... lookup methods.
//!
//! All trait methods are default-implemented. The macro emits the
//! per-model glue (PK accessor, attribute application, into-active-model
//! conversion) and trivial `impl ::suprnova::eloquent::Model for #struct {}`
//! lines.
//!
//! ## delete vs force_delete
//!
//! T4 ships hard-delete only — both methods call SeaORM's DELETE. T10
//! introduces soft-deletes; once that lands, `delete` honours the
//! `soft_deletes` attribute (sets `deleted_at` instead of removing the
//! row) while `force_delete` always removes the row.

use std::collections::HashMap;
use std::hash::Hash;

use async_trait::async_trait;
use sea_orm::{
    ColumnTrait, EntityTrait, IntoActiveModel, PrimaryKeyToColumn, PrimaryKeyTrait, QueryFilter,
};
// `find_many` calls `<Self::Entity as EntityTrait>::PrimaryKey::iter()`.
// `iter` lives on `IntoEnumIterator`, brought in via `PrimaryKeyTrait`'s
// `Iterable` supertrait. Importing it explicitly so the call resolves
// regardless of supertrait-method-resolution edge cases.
use sea_orm::strum::IntoEnumIterator;
use serde::Serialize;
use serde::de::DeserializeOwned;

// Direct `DB::connection()` calls have been replaced with
// `crate::database::transaction::ExecutorChoice::resolve()` so every
// Model CRUD path honours an active `DB::transaction` scope without
// callers threading a tx handle through every method.
use crate::eloquent::EloquentModel;
use crate::eloquent::attrs::Attrs;
use crate::eloquent::builder::Builder;
use crate::eloquent::collection::Collection;
use crate::eloquent::events::ModelEventHooks;
use crate::eloquent::fillable::Fillable;
use crate::error::FrameworkError;

/// The Eloquent CRUD lifecycle. Auto-implemented for every
/// `#[suprnova::model]` struct.
///
/// Method semantics mirror Laravel's `Illuminate\Database\Eloquent\Model`
/// where possible. Divergences are flagged in the rustdoc and in
/// `docs/superpowers/specs/phase-10/phase-10a/01-crud.md`.
#[async_trait]
pub trait Model:
    EloquentModel
    + Send
    + Sync
    + Sized
    + Clone
    + Serialize
    + DeserializeOwned
    + ModelEventHooks
    + 'static
where
    Self: From<<Self::Entity as EntityTrait>::Model>,
    <Self::Entity as EntityTrait>::Model: From<Self>
        + IntoActiveModel<<Self::Entity as EntityTrait>::ActiveModel>
        + Serialize
        + Send
        + Sync,
    <Self::Entity as EntityTrait>::ActiveModel: Send,
    <<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Primary-key column name. The macro emits the value from the
    /// `primary_key = "..."` attribute (default `"id"`).
    fn primary_key_name() -> &'static str {
        "id"
    }

    /// Phase 10C T12 — the per-model default connection name. Returns
    /// `None` for models that don't declare `#[model(connection =
    /// "...")]`; the macro overrides this when the attribute is set,
    /// returning `Some(<literal>)`.
    ///
    /// Consulted by [`crate::database::transaction::ExecutorChoice::resolve_read`]
    /// / [`resolve_write`](crate::database::transaction::ExecutorChoice::resolve_write)
    /// as step 4 of the routing chain — after the per-builder
    /// `on(name)` override but before `__read_replica__` auto-routing.
    /// `Some("__primary__")` short-circuits to
    /// [`crate::DB::connection`] without consulting the registry; any
    /// other name routes through
    /// [`crate::database::ConnectionRegistry::get`].
    fn default_connection_name() -> ::core::option::Option<&'static str> {
        ::core::option::Option::None
    }

    /// Per-model mass-assignment guard. The macro's Task 4 emission
    /// returns `Fillable::guarded(vec![PRIMARY_KEY])`; Task 6 wires
    /// `fillable = [...]` / `guarded = [...]` attributes.
    fn fillable_filter() -> Fillable;

    /// Fallible hydration of an inner SeaORM row into this model — the
    /// `?`-propagating analogue of the infallible
    /// `From<<Self::Entity>::Model>` bridge the macro also emits.
    ///
    /// The framework's own read paths (`find`, `find_many`, `all`,
    /// [`Builder::get`](crate::eloquent::Builder), ...) route through
    /// this method so a cast that fails to decode a stored value — a
    /// corrupt column, a deprecated enum variant left in old rows,
    /// schema drift — surfaces as a recoverable [`FrameworkError`]
    /// rather than a panic. That matters off the HTTP path: a queue
    /// worker, the scheduler, or a CLI command has no panic-recovery
    /// middleware to turn a panic into a 500, so an unguarded panic
    /// there tears down the task.
    ///
    /// The infallible `From` impl is retained as an ergonomic escape
    /// hatch (`let u: User = row.into()`); it panics on the same
    /// failure with a field-named diagnostic. The default below
    /// delegates to it so non-`#[suprnova::model]` types that satisfy
    /// the trait bounds still compile; the macro overrides this with
    /// the per-field `Cast::from_storage` form that propagates via `?`.
    fn try_from_storage(row: <Self::Entity as EntityTrait>::Model) -> Result<Self, FrameworkError> {
        Ok(Self::from(row))
    }

    /// Fallible dehydration of this model into its inner SeaORM row —
    /// the `?`-propagating analogue of the infallible
    /// `From<Self> for <Self::Entity>::Model` bridge.
    ///
    /// The framework's write paths (`save`, `update`, `delete`,
    /// `force_delete`, and their `_with_tx` variants) route through
    /// this so a cast that fails to encode a runtime value becomes a
    /// recoverable [`FrameworkError`] instead of a panic. See
    /// [`Self::try_from_storage`] for the off-the-HTTP-path rationale;
    /// the macro overrides this with the per-field `Cast::to_storage`
    /// form.
    fn try_into_storage(self) -> Result<<Self::Entity as EntityTrait>::Model, FrameworkError> {
        Ok(self.into())
    }

    /// Phase 10C T5b — read this row's field by column name and
    /// serialise it to a `serde_json::Value`. Returns `None` when the
    /// column name doesn't match any declared field on the model (and
    /// when the per-field serialisation fails, which the macro's
    /// arms lower to `None`).
    ///
    /// The default returns `None` so non-`#[suprnova::model]` types
    /// that meet the supertrait bounds (rare — almost nothing else
    /// satisfies them) don't break. The macro overrides this with one
    /// match arm per declared column field.
    ///
    /// Powers the string-keyed surface on
    /// [`Collection<M>`](crate::eloquent::Collection) —
    /// `pluck("col")`, `group_by("col")`, `sort_by("col")`,
    /// `where_eq("col", v)`, `sum::<T>("col")`, etc. The macro emission
    /// lives in `suprnova-macros/src/model/serialization.rs`.
    fn field_value(&self, _name: &str) -> ::core::option::Option<serde_json::Value> {
        ::core::option::Option::None
    }

    /// Phase 10C T6 — serialise this row to a JSON object.
    ///
    /// Default implementation serialises the whole struct via
    /// `serde_json::to_value(self)` and explicitly removes the
    /// macro-injected `__eager` / `__pivot` scratch fields. Both
    /// fields carry `#[serde(skip)]` on the struct definition, so the
    /// removal is belt-and-braces — it pins the [Phase 10B P6
    /// contract](../../docs/superpowers/specs/phase-10/phase-10b.md)
    /// (eager-load cache stays out of serialisation) even against a
    /// hypothetical future model with a hand-rolled `Serialize` impl.
    ///
    /// The macro overrides this when the model declares
    /// `hidden = [...]`, `visible = [...]`, or `appends = [...]` on
    /// `#[suprnova::model]`. The override applies those filters in
    /// Laravel order: visible (whitelist) → hidden (denylist) →
    /// appends (accessor injection, runs after filters so appends
    /// always show up even if they share a name with a hidden field).
    fn to_array(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let Some(m) = v.as_object_mut() {
            m.remove("__eager");
            m.remove("__pivot");
        }
        v
    }

    /// Phase 10C T6 — serialise this row to a JSON string. Delegates
    /// to [`Self::to_array`] so the same hidden/visible/appends
    /// filters apply when callers reach for the string shape directly.
    fn to_json(&self) -> String {
        serde_json::to_string(&self.to_array()).unwrap_or_default()
    }

    /// Phase 10C T6 — append-accessor dispatcher. The macro overrides
    /// this with a `match` block when `appends = [...]` is non-empty,
    /// dispatching each declared name to the user's
    /// `#[suprnova::accessor]`-tagged method. The default returns
    /// `None` for every name, which keeps the [`Self::to_array`]
    /// override branch a no-op for models that don't declare appends.
    #[doc(hidden)]
    fn __append_accessor(&self, _name: &str) -> ::core::option::Option<serde_json::Value> {
        ::core::option::Option::None
    }

    /// Look up a row by primary key. `None` if no row matches.
    ///
    /// The trait default uses SeaORM's `find_by_id` directly — no
    /// global scopes apply. Models that declare `#[model(soft_deletes)]`
    /// receive an inherent `find` override emitted by the macro that
    /// routes through [`Self::query`] (which applies the
    /// `deleted_at IS NULL` filter); the inherent shadows the trait
    /// default for the soft-delete path. Callers that need to bypass
    /// the scope use `Self::with_trashed()` instead.
    ///
    /// Dispatches `Retrieving` before the SELECT and `Retrieved`
    /// when a row is hydrated (no dispatch when the row is missing).
    async fn find<K>(id: K) -> Result<Option<Self>, FrameworkError>
    where
        K: Into<<<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType> + Send,
    {
        Self::__dispatch_retrieving().await?;
        // T11/T12: route through resolve_read so the read honours any
        // ambient `DB::transaction` closure scope, per-model
        // `connection = "..."` default, and `__read_replica__`
        // auto-routing. No builder-level overrides at this layer —
        // `Model::find` doesn't take a Builder.
        let exec = crate::database::transaction::ExecutorChoice::resolve_read(
            None,
            None,
            Self::default_connection_name(),
        )
        .await?;
        let row = match &exec {
            crate::database::transaction::ExecutorChoice::Tx(t) => {
                Self::Entity::find_by_id(id).one(t.as_ref()).await
            }
            crate::database::transaction::ExecutorChoice::Pool(c) => {
                Self::Entity::find_by_id(id).one(c.inner()).await
            }
        }
        .map_err(|e| FrameworkError::database(e.to_string()))?;
        let hydrated = row.map(Self::try_from_storage).transpose()?;
        if let Some(ref m) = hydrated {
            Self::__dispatch_retrieved(m).await?;
        }
        Ok(hydrated)
    }

    /// Look up a row by primary key. Returns `FrameworkError::ModelNotFound`
    /// (HTTP 404) when no row matches.
    async fn find_or_fail<K>(id: K) -> Result<Self, FrameworkError>
    where
        K: Into<<<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType>
            + std::fmt::Debug
            + Copy
            + Send,
    {
        match Self::find(id).await? {
            Some(m) => Ok(m),
            None => Err(FrameworkError::not_found(format!(
                "{} with {} = {:?} not found",
                std::any::type_name::<Self>(),
                Self::primary_key_name(),
                id
            ))),
        }
    }

    /// Fetch every row whose PK is in `ids`. Result preserves the
    /// order of `ids` (not the database's natural order). Unmatched
    /// IDs are silently dropped.
    ///
    /// Dispatches `Retrieving` once before the SELECT and
    /// `Retrieved` once per hydrated row.
    async fn find_many<I, K>(ids: I) -> Result<Vec<Self>, FrameworkError>
    where
        I: IntoIterator<Item = K> + Send,
        I::IntoIter: Send,
        K: Into<<<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType>
            + Clone
            + Send,
        <<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
            Hash + Eq + Clone,
    {
        let id_vec: Vec<_> = ids.into_iter().map(|k| k.into()).collect();
        if id_vec.is_empty() {
            return Ok(Vec::new());
        }
        Self::__dispatch_retrieving().await?;
        let pk = <Self::Entity as EntityTrait>::PrimaryKey::iter()
            .next()
            .expect("model has at least one primary-key column");
        // T11/T12: route through resolve_read.
        let exec = crate::database::transaction::ExecutorChoice::resolve_read(
            None,
            None,
            Self::default_connection_name(),
        )
        .await?;
        let rows = match &exec {
            crate::database::transaction::ExecutorChoice::Tx(t) => {
                Self::Entity::find()
                    .filter(pk.into_column().is_in(id_vec.clone()))
                    .all(t.as_ref())
                    .await
            }
            crate::database::transaction::ExecutorChoice::Pool(c) => {
                Self::Entity::find()
                    .filter(pk.into_column().is_in(id_vec.clone()))
                    .all(c.inner())
                    .await
            }
        }
        .map_err(|e| FrameworkError::database(e.to_string()))?;

        let mut by_id: HashMap<_, _> = rows
            .into_iter()
            .map(|row| {
                let model = Self::try_from_storage(row)?;
                Ok((model.primary_key_value(), model))
            })
            .collect::<Result<HashMap<_, _>, FrameworkError>>()?;
        let ordered: Vec<Self> = id_vec
            .into_iter()
            .filter_map(|id| by_id.remove(&id))
            .collect();
        for row in &ordered {
            Self::__dispatch_retrieved(row).await?;
        }
        Ok(ordered)
    }

    /// Fetch every row in the table.
    ///
    /// Dispatches `Retrieving` once before the SELECT and
    /// `Retrieved` once per hydrated row.
    ///
    /// Returns a [`Collection<Self>`](crate::eloquent::Collection) so
    /// the result composes with the model-aware string-keyed surface
    /// (`pluck("col")`, `group_by("col")`, `sum::<T>("col")`, ...). The
    /// inner `Vec` is reachable via `.into_vec()` for call sites that
    /// need explicit `Vec` semantics; slice-shape access (`.iter()`,
    /// `.len()`, indexing, `for row in &collection`) works directly
    /// via `Deref<Target = [Self]>`.
    async fn all() -> Result<Collection<Self>, FrameworkError> {
        Self::__dispatch_retrieving().await?;
        // T11/T12: route through resolve_read.
        let exec = crate::database::transaction::ExecutorChoice::resolve_read(
            None,
            None,
            Self::default_connection_name(),
        )
        .await?;
        let rows = match &exec {
            crate::database::transaction::ExecutorChoice::Tx(t) => {
                Self::Entity::find().all(t.as_ref()).await
            }
            crate::database::transaction::ExecutorChoice::Pool(c) => {
                Self::Entity::find().all(c.inner()).await
            }
        }
        .map_err(|e| FrameworkError::database(e.to_string()))?;
        let out: Vec<Self> = rows
            .into_iter()
            .map(Self::try_from_storage)
            .collect::<Result<Vec<_>, _>>()?;
        for row in &out {
            Self::__dispatch_retrieved(row).await?;
        }
        Ok(Collection::from_vec(out))
    }

    /// Start a new builder against this model. Phase 10C T4 layers
    /// registered global scopes onto the fresh builder so every read
    /// path is scoped by default; callers opt out per-type with
    /// [`Builder::without_global_scope::<S>`] or all-at-once with
    /// [`Builder::without_global_scopes`].
    ///
    /// The scope registry is keyed by `TypeId::of::<Self>()`. The
    /// `Model: 'static` supertrait bound makes that lookup well-defined
    /// for every concrete `#[suprnova::model]` struct.
    ///
    /// [`Builder::without_global_scope::<S>`]: crate::eloquent::Builder::without_global_scope
    /// [`Builder::without_global_scopes`]: crate::eloquent::Builder::without_global_scopes
    fn query() -> Builder<Self> {
        crate::eloquent::scopes::ScopeRegistry::apply_to::<Self>(Builder::new())
    }

    /// Mass-create a row from the given attributes. Attributes are
    /// filtered through [`Self::fillable_filter`] before the SeaORM
    /// ActiveModel is built.
    ///
    /// ## Lifecycle events (Phase 10C T1)
    ///
    /// Dispatched in this order:
    ///
    /// 1. `Creating { attrs }` — cancellable
    /// 2. `Saving { attrs, is_creating: true }` — cancellable
    /// 3. *INSERT lands*
    /// 4. `Created { model }`
    /// 5. `Saved { model }`
    ///
    /// A listener that cancels at (1) or (2) aborts the operation
    /// with `FrameworkError::bad_request(reason)`; the INSERT never
    /// runs. Listeners on (1) / (2) may mutate the in-flight `Attrs`
    /// through the `Arc<tokio::sync::Mutex<Attrs>>` they receive.
    async fn create(attrs: Attrs) -> Result<Self, FrameworkError> {
        let filtered = Self::fillable_filter().apply(attrs);
        // Wrap the filtered attrs in an Arc<Mutex<_>> so cancellable
        // listeners (Creating, Saving) can mutate the in-flight
        // values before the INSERT runs.
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(filtered));
        Self::__dispatch_creating(shared.clone()).await?;
        Self::__dispatch_saving(shared.clone(), true).await?;

        // Read the (possibly mutated) attrs back out of the mutex
        // before consuming them to build the ActiveModel. The
        // Arc<Mutex<_>> handle is dropped once we leave this scope.
        let final_attrs = shared.lock().await.clone();
        let am = Self::active_model_from_attrs(final_attrs)?;
        // T11/T12: route through resolve_write — insert lands in the
        // active transaction when called inside `DB::transaction`,
        // honours per-model `connection = "..."`, and skips
        // `__read_replica__` (writes always go to primary unless the
        // model explicitly opts elsewhere).
        let exec = crate::database::transaction::ExecutorChoice::resolve_write(
            None,
            None,
            Self::default_connection_name(),
        )
        .await?;
        let inserted = exec
            .insert_active(am)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let row = Self::try_from_storage(inserted)?;

        Self::__dispatch_created(&row).await?;
        Self::__dispatch_saved(&row).await?;
        Ok(row)
    }

    /// Persist any field changes on this row. The full row is sent to
    /// the database — T4 doesn't track per-field dirty state.
    ///
    /// ## Lifecycle events (Phase 10C T1)
    ///
    /// 1. `Updating { previous, attrs }` — cancellable
    /// 2. `Saving { attrs, is_creating: false }` — cancellable
    /// 3. *UPDATE lands*
    /// 4. `Updated { previous, current }`
    /// 5. `Saved { model: current }`
    ///
    /// The `previous` snapshot is `self` at call time; `current` is
    /// the row as the database has it after the UPDATE. A listener
    /// that cancels at (1) or (2) aborts with
    /// `FrameworkError::bad_request(reason)`.
    async fn save(&self) -> Result<(), FrameworkError> {
        // Serialize the in-memory model to an Attrs map so listeners
        // see the "what's about to be written" payload through the
        // same Arc<Mutex<Attrs>> shape they see on create.
        let attrs_value = serde_json::to_value(self).map_err(|e| {
            FrameworkError::internal(format!("save: serialize self for Saving event: {e}"))
        })?;
        let attrs = Attrs::from(attrs_value);
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(attrs));

        Self::__dispatch_updating(self, shared.clone()).await?;
        Self::__dispatch_saving(shared.clone(), false).await?;

        // Audit HIGH `eloquent` #2 — read the (possibly listener-
        // mutated) attrs back from the shared map and overlay onto
        // the ActiveModel. The earlier code built the ActiveModel
        // straight from `self.clone()` and silently dropped any
        // listener mutations to the Updating / Saving payload.
        let final_attrs = shared.lock().await.clone();
        let mut am = self.clone().into_active_model_for_update()?;
        Self::apply_attrs_to_active_model(&mut am, final_attrs)?;
        // T11/T12: route through resolve_write.
        let exec = crate::database::transaction::ExecutorChoice::resolve_write(
            None,
            None,
            Self::default_connection_name(),
        )
        .await?;
        let updated = exec
            .update_active(am)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let current = Self::try_from_storage(updated)?;

        Self::__dispatch_updated(self, &current).await?;
        Self::__dispatch_saved(&current).await?;
        Ok(())
    }

    /// Apply a partial attribute set and persist. Attributes are
    /// filtered through [`Self::fillable_filter`] first.
    ///
    /// ## Lifecycle events (Phase 10C T1)
    ///
    /// Same event sequence as [`Self::save`] — `Updating` /
    /// `Saving { is_creating: false }` before the UPDATE, then
    /// `Updated` / `Saved` after.
    async fn update(self, attrs: Attrs) -> Result<Self, FrameworkError> {
        let previous = self.clone();
        let filtered = Self::fillable_filter().apply(attrs);
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(filtered));

        Self::__dispatch_updating(&previous, shared.clone()).await?;
        Self::__dispatch_saving(shared.clone(), false).await?;

        let final_attrs = shared.lock().await.clone();
        let row = self.try_into_storage()?;
        let mut am = row.into_active_model();
        Self::apply_attrs_to_active_model(&mut am, final_attrs)?;
        // T11/T12: route through resolve_write.
        let exec = crate::database::transaction::ExecutorChoice::resolve_write(
            None,
            None,
            Self::default_connection_name(),
        )
        .await?;
        let updated = exec
            .update_active(am)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let current = Self::try_from_storage(updated)?;

        Self::__dispatch_updated(&previous, &current).await?;
        Self::__dispatch_saved(&current).await?;
        Ok(current)
    }

    /// Delete this row. The trait default performs a hard DELETE.
    /// Models annotated `#[suprnova::model(soft_deletes)]` get an
    /// inherent override that flips this to an UPDATE SET deleted_at
    /// (see `suprnova-macros/src/model/derive_eloquent.rs`).
    ///
    /// ## Lifecycle events (Phase 10C T1)
    ///
    /// 1. `Deleting { model, is_force: false }` — cancellable
    /// 2. *DELETE lands*
    /// 3. `Deleted { model, is_force: false }`
    ///
    /// Soft-delete models override the inherent `delete` to also
    /// dispatch `Trashed { model }` after step 2.
    async fn delete(self) -> Result<(), FrameworkError> {
        Self::__dispatch_deleting(&self, false).await?;

        let snapshot = self.clone();
        let row = self.try_into_storage()?;
        let am = row.into_active_model();
        // T11/T12: route through resolve_write.
        let exec = crate::database::transaction::ExecutorChoice::resolve_write(
            None,
            None,
            Self::default_connection_name(),
        )
        .await?;
        exec.delete_active(am)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        Self::__dispatch_deleted(&snapshot, false).await?;
        Ok(())
    }

    /// Hard-delete this row, bypassing any soft-delete override. For
    /// non-soft-delete models this is identical to `delete`. Models
    /// annotated `#[suprnova::model(soft_deletes)]` get an inherent
    /// override that ALSO fires `ForceDeleting` / `ForceDeleted` and
    /// `Deleting { is_force: true }` / `Deleted { is_force: true }`
    /// (Trashed is NOT fired — the row is gone, not tombstoned).
    async fn force_delete(self) -> Result<(), FrameworkError> {
        Self::__dispatch_deleting(&self, true).await?;
        Self::__dispatch_force_deleting(&self).await?;

        let snapshot = self.clone();
        let row = self.try_into_storage()?;
        let am = row.into_active_model();
        // T11/T12: route through resolve_write.
        let exec = crate::database::transaction::ExecutorChoice::resolve_write(
            None,
            None,
            Self::default_connection_name(),
        )
        .await?;
        exec.delete_active(am)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        Self::__dispatch_force_deleted(&snapshot).await?;
        Self::__dispatch_deleted(&snapshot, true).await?;
        Ok(())
    }

    // ---- Phase 10C T11 — manual-transaction shims --------------------
    //
    // `DB::begin_transaction()` returns a `Transaction` handle and
    // does NOT install the [`CURRENT_TX`] task-local; callers must
    // opt every operation into the transaction explicitly. These
    // `_with_tx` methods are the per-Model entry points; pair them
    // with `Builder::with_tx(&tx)` for read paths.
    //
    // The shims route through `ExecutorChoice::from_tx(tx)` which
    // bypasses CURRENT_TX consultation entirely — the explicit
    // handle is authoritative. Lifecycle events still fire in the
    // same order as the non-tx variant.

    /// Persist this row's in-memory state through `tx`. Same lifecycle
    /// event sequence as [`Self::save`] (`Updating` → `Saving` →
    /// UPDATE → `Updated` → `Saved`). Used with
    /// [`DB::begin_transaction`](crate::DB::begin_transaction) when the
    /// closure form doesn't fit the caller's control flow.
    async fn save_with_tx(&self, tx: &crate::database::Transaction) -> Result<(), FrameworkError> {
        let attrs_value = serde_json::to_value(self).map_err(|e| {
            FrameworkError::internal(format!(
                "save_with_tx: serialize self for Saving event: {e}"
            ))
        })?;
        let attrs = Attrs::from(attrs_value);
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(attrs));

        Self::__dispatch_updating(self, shared.clone()).await?;
        Self::__dispatch_saving(shared.clone(), false).await?;

        // Audit HIGH `eloquent` #2 — match `save()`'s lifecycle: read
        // the listener-mutated attrs back and apply them to the
        // ActiveModel before the UPDATE fires.
        let final_attrs = shared.lock().await.clone();
        let mut am = self.clone().into_active_model_for_update()?;
        Self::apply_attrs_to_active_model(&mut am, final_attrs)?;
        let exec = crate::database::transaction::ExecutorChoice::from_tx(tx);
        let updated = exec
            .update_active(am)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let current = Self::try_from_storage(updated)?;

        Self::__dispatch_updated(self, &current).await?;
        Self::__dispatch_saved(&current).await?;
        Ok(())
    }

    /// Apply `attrs` to this row through `tx`. Mirrors
    /// [`Self::update`] event-for-event but pins the SQL to the
    /// supplied transaction. Returns the updated row.
    async fn update_with_tx(
        self,
        tx: &crate::database::Transaction,
        attrs: Attrs,
    ) -> Result<Self, FrameworkError> {
        let previous = self.clone();
        let filtered = Self::fillable_filter().apply(attrs);
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(filtered));

        Self::__dispatch_updating(&previous, shared.clone()).await?;
        Self::__dispatch_saving(shared.clone(), false).await?;

        let final_attrs = shared.lock().await.clone();
        let row = self.try_into_storage()?;
        let mut am = row.into_active_model();
        Self::apply_attrs_to_active_model(&mut am, final_attrs)?;
        let exec = crate::database::transaction::ExecutorChoice::from_tx(tx);
        let updated = exec
            .update_active(am)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let current = Self::try_from_storage(updated)?;

        Self::__dispatch_updated(&previous, &current).await?;
        Self::__dispatch_saved(&current).await?;
        Ok(current)
    }

    /// Delete this row through `tx`. Soft-delete models override the
    /// inherent `delete` to apply tombstone semantics on the trait
    /// path, but this trait-level shim performs a hard DELETE
    /// regardless. Use [`Self::force_delete_with_tx`] for symmetry
    /// when you want the operation to read as "definitely remove" at
    /// the call site.
    async fn delete_with_tx(self, tx: &crate::database::Transaction) -> Result<(), FrameworkError> {
        Self::__dispatch_deleting(&self, false).await?;

        let snapshot = self.clone();
        let row = self.try_into_storage()?;
        let am = row.into_active_model();
        let exec = crate::database::transaction::ExecutorChoice::from_tx(tx);
        exec.delete_active(am)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        Self::__dispatch_deleted(&snapshot, false).await?;
        Ok(())
    }

    /// Create a row through `tx`. Phase 10C audit-fix AF5 closes the
    /// manual-transaction shim inventory — every CRUD entry point on
    /// [`Self`] except `create` previously had a `*_with_tx`
    /// counterpart, so a user inside [`DB::begin_transaction`] who
    /// wanted to `create` had to fall back to building an
    /// `ActiveModel` by hand and reaching for raw SeaORM. This shim
    /// mirrors [`Self::create`] event-for-event (`Creating` → `Saving`
    /// → INSERT → `Created` → `Saved`) but pins the INSERT to `tx`.
    async fn create_with_tx(
        tx: &crate::database::Transaction,
        attrs: Attrs,
    ) -> Result<Self, FrameworkError> {
        let filtered = Self::fillable_filter().apply(attrs);
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(filtered));
        Self::__dispatch_creating(shared.clone()).await?;
        Self::__dispatch_saving(shared.clone(), true).await?;

        let final_attrs = shared.lock().await.clone();
        let am = Self::active_model_from_attrs(final_attrs)?;
        let exec = crate::database::transaction::ExecutorChoice::from_tx(tx);
        let inserted = exec
            .insert_active(am)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let row = Self::try_from_storage(inserted)?;

        Self::__dispatch_created(&row).await?;
        Self::__dispatch_saved(&row).await?;
        Ok(row)
    }

    /// Force-delete this row through `tx`. Mirrors
    /// [`Self::force_delete`] event-for-event but pins the DELETE to
    /// the supplied transaction.
    async fn force_delete_with_tx(
        self,
        tx: &crate::database::Transaction,
    ) -> Result<(), FrameworkError> {
        Self::__dispatch_deleting(&self, true).await?;
        Self::__dispatch_force_deleting(&self).await?;

        let snapshot = self.clone();
        let row = self.try_into_storage()?;
        let am = row.into_active_model();
        let exec = crate::database::transaction::ExecutorChoice::from_tx(tx);
        exec.delete_active(am)
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        Self::__dispatch_force_deleted(&snapshot).await?;
        Self::__dispatch_deleted(&snapshot, true).await?;
        Ok(())
    }

    /// Reload this row from the database, mutating self in place. The
    /// PK is preserved; every other column reflects the latest state.
    async fn refresh(&mut self) -> Result<(), FrameworkError> {
        let pk = self.primary_key_value();
        let fresh = Self::find(pk)
            .await?
            .ok_or_else(|| FrameworkError::not_found("refresh: row no longer exists"))?;
        *self = fresh;
        Ok(())
    }

    /// Return a freshly-fetched copy of this row without mutating
    /// `self`. `None` if the row was deleted in the interim.
    async fn fresh(&self) -> Result<Option<Self>, FrameworkError> {
        Self::find(self.primary_key_value()).await
    }

    /// Build an unsaved clone with the PK reset and any auto-managed
    /// columns cleared. Caller saves explicitly.
    ///
    /// ## Lifecycle events (Phase 10C T13)
    ///
    /// Fires `Replicating { source, replica }` AFTER the in-memory
    /// clone is constructed and BEFORE this method returns. The
    /// `replica` field is an `Arc<tokio::sync::Mutex<Self>>` so
    /// listeners can mutate the replica (clear timestamps, reset
    /// flags, append a `(copy)` prefix to the title, etc.) before
    /// the caller sees it.
    async fn replicate(&self) -> Result<Self, FrameworkError>
    where
        Self: ReplicateExt,
    {
        // ReplicateExt::replicate_with takes Vec<String>; match the
        // element type. `Vec::<&str>::new()` would compile-error here
        // even though the vec is empty.
        let copy = self.replicate_with(Vec::<String>::new());
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(copy));
        Self::__dispatch_replicating(self, shared.clone()).await?;
        Ok(shared.lock().await.clone())
    }

    /// Like [`Self::replicate`] but also clears every column whose
    /// name appears in `except`.
    ///
    /// Fires `Replicating` with the same `Arc<Mutex<Self>>` contract
    /// as [`Self::replicate`].
    async fn replicate_except<I, S>(&self, except: I) -> Result<Self, FrameworkError>
    where
        I: IntoIterator<Item = S> + Send,
        S: AsRef<str> + Send,
        Self: ReplicateExt,
    {
        let copy = self.replicate_with(
            except
                .into_iter()
                .map(|s| s.as_ref().to_string())
                .collect::<Vec<_>>(),
        );
        let shared = std::sync::Arc::new(tokio::sync::Mutex::new(copy));
        Self::__dispatch_replicating(self, shared.clone()).await?;
        Ok(shared.lock().await.clone())
    }

    /// Replicate this row into a different model type. Suprnova
    /// divergence from Laravel — Laravel can't do this because PHP
    /// has no static types. The transfer goes via JSON: `self` is
    /// serialised to a `serde_json::Value`, then deserialised into
    /// `T`. After deserialisation, the target's PK is reset to its
    /// `Default::default()` so the replica is genuinely unsaved.
    ///
    /// ## Field-shape contract
    ///
    /// `T` must accept every field `Self` serialises. Concretely:
    /// fields present on `T` but absent from `Self` must be
    /// `Option<_>` or annotated `#[serde(default)]`; otherwise serde
    /// will fail the round-trip with a "missing field" error. Fields
    /// present on `Self` but absent from `T` are silently dropped.
    /// For the same-shape case (e.g. `User` -> `UserDraft` where both
    /// carry the same columns), no extra annotations are needed.
    ///
    /// ## No `Replicating` event for cross-type replication
    ///
    /// `Replicating` is per-source-type (the event struct holds an
    /// `Arc<Mutex<Self>>`). For cross-type replication the source's
    /// `Replicating` listener would receive an `Arc<Mutex<Self>>`,
    /// not `Arc<Mutex<T>>` — which can't mutate the cross-type
    /// replica that's about to be returned. We deliberately skip the
    /// dispatch: callers wanting per-T setup should run it on the
    /// returned `T` value before calling `T::save`. Inside `T::save`
    /// the normal `Saving` / `Created` chain still fires.
    async fn replicate_into<T>(&self) -> Result<T, FrameworkError>
    where
        T: Model + DeserializeOwned + Serialize,
        T: From<<T::Entity as EntityTrait>::Model>,
        <T::Entity as EntityTrait>::Model: From<T>
            + IntoActiveModel<<T::Entity as EntityTrait>::ActiveModel>
            + Serialize
            + Send
            + Sync,
        <T::Entity as EntityTrait>::ActiveModel: Send,
        <<T::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
            Send + Into<sea_orm::Value>,
    {
        let json = serde_json::to_value(self)
            .map_err(|e| FrameworkError::internal(format!("replicate_into serialize: {e}")))?;
        // The PK from `self` will land in `T`'s same-named PK field
        // during deserialisation. We don't strip it from the JSON
        // (that would break round-tripping when `T`'s PK field isn't
        // `Option`/`#[serde(default)]`); instead, we reset the PK on
        // the replica immediately after deserialisation so the result
        // is genuinely unsaved.
        let mut replica: T = serde_json::from_value(json)
            .map_err(|e| FrameworkError::internal(format!("replicate_into deserialize: {e}")))?;
        replica.reset_primary_key();
        Ok(replica)
    }

    /// Atomic `UPDATE table SET col = col + by WHERE pk = ?`. Safe
    /// against concurrent updates — no read-modify-write race.
    ///
    /// # Security
    ///
    /// `column` is interpolated as a SQL identifier (not a bound
    /// parameter — SQL doesn't allow that). The call validates
    /// `column` via [`crate::database::validate_identifier`] before
    /// rendering, so attacker-controlled strings are rejected at the
    /// I/O boundary with [`FrameworkError`]. Same contract as
    /// Laravel's `Model::increment($column, $by)`.
    async fn increment(&self, column: &str, by: i64) -> Result<(), FrameworkError> {
        // Audit HIGH `eloquent` #1 — column is interpolated raw into
        // the SQL string and cannot be parameterised. Validate
        // against the framework's SQL identifier rules before render.
        crate::database::validate_identifier(column)?;
        let table = Self::TABLE;
        let pk_name = Self::primary_key_name();
        let pk_value = self.primary_key_value_json();
        let sql = format!("UPDATE {table} SET {column} = {column} + ? WHERE {pk_name} = ?");
        // T11/T12: route through resolve_write.
        let exec = crate::database::transaction::ExecutorChoice::resolve_write(
            None,
            None,
            Self::default_connection_name(),
        )
        .await?;
        let backend = exec.backend();
        exec.run(sea_orm::Statement::from_sql_and_values(
            backend,
            &sql,
            vec![by.into(), json_value_to_sea_value(&pk_value)],
        ))
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(())
    }

    /// Atomic `UPDATE table SET col = col - by WHERE pk = ?`. Sugar
    /// over `increment(column, -by)`.
    async fn decrement(&self, column: &str, by: i64) -> Result<(), FrameworkError> {
        self.increment(column, -by).await
    }

    // --- Hooks the macro-generated impl fills in ---

    /// Return this row's primary-key value (typed, for SeaORM lookups).
    fn primary_key_value(
        &self,
    ) -> <<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType;

    /// Return this row's primary-key value as JSON (for `increment` /
    /// `decrement` SQL binding without exposing the typed PK to the
    /// trait surface).
    fn primary_key_value_json(&self) -> serde_json::Value;

    /// Reset the PK to its `Default::default()` value. Used by
    /// `replicate_into` to ensure the replica is unsaved.
    fn reset_primary_key(&mut self);

    /// Build a fresh SeaORM `ActiveModel` from `attrs`. The PK is
    /// left as `NotSet` so `auto_increment` kicks in.
    fn active_model_from_attrs(
        attrs: Attrs,
    ) -> Result<<Self::Entity as EntityTrait>::ActiveModel, FrameworkError>;

    /// Apply `attrs` to an existing `ActiveModel`. Used by `update`
    /// to overlay partial changes onto the full row before SeaORM
    /// fires the UPDATE.
    fn apply_attrs_to_active_model(
        am: &mut <Self::Entity as EntityTrait>::ActiveModel,
        attrs: Attrs,
    ) -> Result<(), FrameworkError>;

    /// Materialise `self` into an `ActiveModel` for `save`. The PK is
    /// marked as Unchanged (so it acts as the WHERE clause) and every
    /// other column as Set.
    fn into_active_model_for_update(
        self,
    ) -> Result<<Self::Entity as EntityTrait>::ActiveModel, FrameworkError>;
}

/// Cross-cutting helper called from `increment` / `decrement` and
/// from the Builder stub. Centralised here so both call sites stay
/// in sync.
pub fn json_value_to_sea_value(v: &serde_json::Value) -> sea_orm::Value {
    use sea_orm::Value;
    match v {
        serde_json::Value::String(s) => Value::String(Some(Box::new(s.clone()))),
        serde_json::Value::Bool(b) => Value::Bool(Some(*b)),
        serde_json::Value::Number(n) if n.is_i64() => Value::BigInt(Some(n.as_i64().unwrap())),
        serde_json::Value::Number(n) if n.is_u64() => {
            // SeaORM has no unsigned 64-bit type that maps cleanly here;
            // fall back to i64 if it fits, else to string.
            n.as_u64()
                .and_then(|u| i64::try_from(u).ok())
                .map(|i| Value::BigInt(Some(i)))
                .unwrap_or_else(|| Value::String(Some(Box::new(n.to_string()))))
        }
        serde_json::Value::Number(n) if n.is_f64() => Value::Double(Some(n.as_f64().unwrap())),
        serde_json::Value::Null => Value::String(None),
        _ => Value::String(Some(Box::new(v.to_string()))),
    }
}

/// Inverse of [`json_value_to_sea_value`] — best-effort conversion of
/// the variants commonly used as cursor / PK boundaries
/// (`Int`/`BigInt`/`Float`/`Double`/`String`/`Uuid`/`Bool`) into a
/// JSON form the Builder's `filter_op` chain can rebind through its
/// own placeholder pipeline.
///
/// Unsigned integers fall back to a stringified form when they don't
/// fit `i64`; rarely-used variants (bytes, decimals, chrono types)
/// stringify too. The full SeaORM `Value` ↔ JSON round-trip is
/// handled by the cursor wire codec in `pagination/cursor.rs`; this
/// helper exists only for the page-fetch boundary, where the value
/// will be rebound by SQLx within microseconds of being JSON-ified.
pub fn sea_value_to_json_loose(v: &sea_orm::Value) -> serde_json::Value {
    use sea_orm::Value;
    use serde_json::Value as J;
    match v {
        Value::Bool(Some(b)) => J::from(*b),
        Value::TinyInt(Some(i)) => J::from(*i),
        Value::SmallInt(Some(i)) => J::from(*i),
        Value::Int(Some(i)) => J::from(*i),
        Value::BigInt(Some(i)) => J::from(*i),
        Value::TinyUnsigned(Some(i)) => J::from(*i),
        Value::SmallUnsigned(Some(i)) => J::from(*i),
        Value::Unsigned(Some(i)) => J::from(*i),
        Value::BigUnsigned(Some(i)) => i64::try_from(*i)
            .map(J::from)
            .unwrap_or_else(|_| J::String(i.to_string())),
        Value::Float(Some(f)) => J::from(*f as f64),
        Value::Double(Some(f)) => J::from(*f),
        Value::String(Some(s)) => J::String((**s).clone()),
        Value::Char(Some(c)) => J::String(c.to_string()),
        Value::Uuid(Some(u)) => J::String(u.to_string()),
        // Datetimes / decimals stringify — they round-trip back through
        // `json_value_to_sea_value` as Value::String, which the dialect
        // adapter then re-binds via SQL string coercion. Sufficient for
        // a cursor boundary comparison since the underlying column
        // already accepts string-shaped binds for these types.
        Value::ChronoDate(Some(d)) => J::String(d.to_string()),
        Value::ChronoTime(Some(t)) => J::String(t.to_string()),
        Value::ChronoDateTime(Some(dt)) => J::String(dt.to_string()),
        Value::ChronoDateTimeUtc(Some(dt)) => {
            J::String(dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
        }
        Value::ChronoDateTimeLocal(Some(dt)) => {
            J::String(dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
        }
        Value::ChronoDateTimeWithTimeZone(Some(dt)) => {
            J::String(dt.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
        }
        Value::Decimal(Some(d)) => J::String(d.to_string()),
        Value::BigDecimal(Some(d)) => J::String(d.to_string()),
        // Null variants (Some=None) or unsupported variants — emit
        // JSON null so the rebind lands on `WHERE col > NULL`. SQL's
        // three-valued logic treats that as "no rows match", which is
        // a safer-than-silent-mismatch failure mode.
        _ => J::Null,
    }
}

/// Macro-generated hook used by [`Model::replicate`] and
/// [`Model::replicate_except`].
pub trait ReplicateExt: Sized {
    /// Build a clone of `self` with the PK reset and every column
    /// named in `except` cleared to its `Default::default()`.
    fn replicate_with(&self, except: Vec<String>) -> Self;
}

/// First-or-... lookup helpers. Split into a separate trait so the
/// macro can emit a one-line `from_attrs_unsaved` hook per model
/// without bloating the [`Model`] surface.
///
/// The bounds duplicate `Model`'s where clause because Rust's trait
/// elaboration doesn't transitively propagate associated-type bounds
/// from a supertrait's where clause to a subtrait's method bodies.
/// Without these, `Self::query()` inside `first_or_create` fails to
/// type-check against the same constraints `Model::query()` is
/// declared with.
#[async_trait]
pub trait FirstOrCreate: Model + Send + Sync
where
    Self: From<<Self::Entity as EntityTrait>::Model> + crate::eloquent::EagerLoadDispatch,
    <Self::Entity as EntityTrait>::Model: From<Self>
        + IntoActiveModel<<Self::Entity as EntityTrait>::ActiveModel>
        + sea_orm::FromQueryResult
        + Serialize
        + Send
        + Sync,
    <Self::Entity as EntityTrait>::ActiveModel: Send,
    <<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Look up a row by `lookup`. If found, return it. Otherwise
    /// create one with `lookup` merged with `extras` and return that.
    async fn first_or_create(lookup: Attrs, extras: Attrs) -> Result<Self, FrameworkError> {
        let existing = Self::query().filter_attrs(&lookup).first().await?;
        if let Some(found) = existing {
            return Ok(found);
        }
        Self::create(lookup.merge(extras)).await
    }

    /// Look up a row by `lookup`. If found, apply `updates` to it and
    /// return. Otherwise create one with `lookup` merged with `updates`
    /// and return that.
    async fn update_or_create(lookup: Attrs, updates: Attrs) -> Result<Self, FrameworkError> {
        let existing = Self::query().filter_attrs(&lookup).first().await?;
        if let Some(found) = existing {
            return found.update(updates).await;
        }
        Self::create(lookup.merge(updates)).await
    }

    /// Look up a row by `lookup`. If found, return it. Otherwise build
    /// an unsaved in-memory instance from `lookup` and return that.
    async fn first_or_new(lookup: Attrs) -> Result<Self, FrameworkError> {
        match Self::query().filter_attrs(&lookup).first().await? {
            Some(found) => Ok(found),
            None => Self::from_attrs_unsaved(lookup),
        }
    }

    /// Look up a row by `lookup`. If found, return it. Otherwise run
    /// `fallback` and return whatever it produces.
    async fn first_or<F, Fut>(lookup: Attrs, fallback: F) -> Result<Self, FrameworkError>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<Self, FrameworkError>> + Send,
    {
        match Self::query().filter_attrs(&lookup).first().await? {
            Some(found) => Ok(found),
            None => fallback().await,
        }
    }

    /// Build an unsaved in-memory instance from `attrs`. Used by
    /// `first_or_new`. The macro fills this in.
    fn from_attrs_unsaved(attrs: Attrs) -> Result<Self, FrameworkError>;
}
