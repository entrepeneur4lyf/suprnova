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
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel,
    PrimaryKeyToColumn, PrimaryKeyTrait, QueryFilter,
};
// `find_many` calls `<Self::Entity as EntityTrait>::PrimaryKey::iter()`.
// `iter` lives on `IntoEnumIterator`, brought in via `PrimaryKeyTrait`'s
// `Iterable` supertrait. Importing it explicitly so the call resolves
// regardless of supertrait-method-resolution edge cases.
use sea_orm::strum::IntoEnumIterator;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::database::DB;
use crate::eloquent::attrs::Attrs;
use crate::eloquent::builder::Builder;
use crate::eloquent::events::ModelEventHooks;
use crate::eloquent::fillable::Fillable;
use crate::eloquent::EloquentModel;
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

    /// Per-model mass-assignment guard. The macro's Task 4 emission
    /// returns `Fillable::guarded(vec![PRIMARY_KEY])`; Task 6 wires
    /// `fillable = [...]` / `guarded = [...]` attributes.
    fn fillable_filter() -> Fillable;

    /// Look up a row by primary key. `None` if no row matches.
    ///
    /// The trait default uses SeaORM's `find_by_id` directly — no
    /// global scopes apply. Models that declare `#[model(soft_deletes)]`
    /// receive an inherent `find` override emitted by the macro that
    /// routes through [`Self::query`] (which applies the
    /// `deleted_at IS NULL` filter); the inherent shadows the trait
    /// default for the soft-delete path. Callers that need to bypass
    /// the scope use `Self::with_trashed()` instead.
    async fn find<K>(id: K) -> Result<Option<Self>, FrameworkError>
    where
        K: Into<<<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType> + Send,
    {
        let db = DB::connection()?;
        let row = Self::Entity::find_by_id(id)
            .one(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(row.map(Self::from))
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
        let db = DB::connection()?;
        let id_vec: Vec<_> = ids.into_iter().map(|k| k.into()).collect();
        if id_vec.is_empty() {
            return Ok(Vec::new());
        }
        let pk = <Self::Entity as EntityTrait>::PrimaryKey::iter()
            .next()
            .expect("model has at least one primary-key column");
        let rows = Self::Entity::find()
            .filter(pk.into_column().is_in(id_vec.clone()))
            .all(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        let mut by_id: HashMap<_, _> = rows
            .into_iter()
            .map(|row| {
                let model = Self::from(row);
                (model.primary_key_value(), model)
            })
            .collect();
        let ordered = id_vec
            .into_iter()
            .filter_map(|id| by_id.remove(&id))
            .collect();
        Ok(ordered)
    }

    /// Fetch every row in the table.
    async fn all() -> Result<Vec<Self>, FrameworkError> {
        let db = DB::connection()?;
        let rows = Self::Entity::find()
            .all(db.inner())
            .await
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        Ok(rows.into_iter().map(Self::from).collect())
    }

    /// Start a new builder against this model. T4 ships a SQL-only
    /// stub; T5 swaps in the full dual-API builder.
    fn query() -> Builder<Self> {
        Builder::new()
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
        let db = DB::connection()?;
        let inserted = <<Self::Entity as EntityTrait>::ActiveModel as ActiveModelTrait>::insert(
            am,
            db.inner(),
        )
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;
        let row = Self::from(inserted);

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

        let am = self.clone().into_active_model_for_update()?;
        let db = DB::connection()?;
        let updated = <<Self::Entity as EntityTrait>::ActiveModel as ActiveModelTrait>::update(
            am,
            db.inner(),
        )
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;
        let current = Self::from(updated);

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
        let row: <Self::Entity as EntityTrait>::Model = self.into();
        let mut am = row.into_active_model();
        Self::apply_attrs_to_active_model(&mut am, final_attrs)?;
        let db = DB::connection()?;
        let updated =
            <<Self::Entity as EntityTrait>::ActiveModel as ActiveModelTrait>::update(am, db.inner())
                .await
                .map_err(|e| FrameworkError::database(e.to_string()))?;
        let current = Self::from(updated);

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
        let row: <Self::Entity as EntityTrait>::Model = self.into();
        let am = row.into_active_model();
        let db = DB::connection()?;
        <<Self::Entity as EntityTrait>::ActiveModel as ActiveModelTrait>::delete(am, db.inner())
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
        let row: <Self::Entity as EntityTrait>::Model = self.into();
        let am = row.into_active_model();
        let db = DB::connection()?;
        <<Self::Entity as EntityTrait>::ActiveModel as ActiveModelTrait>::delete(am, db.inner())
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
    fn replicate(&self) -> Self
    where
        Self: ReplicateExt,
    {
        // ReplicateExt::replicate_with takes Vec<String>; match the
        // element type. `Vec::<&str>::new()` would compile-error here
        // even though the vec is empty.
        self.replicate_with(Vec::<String>::new())
    }

    /// Like [`Self::replicate`] but also clears every column whose
    /// name appears in `except`.
    fn replicate_except<I, S>(&self, except: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
        Self: ReplicateExt,
    {
        self.replicate_with(
            except
                .into_iter()
                .map(|s| s.as_ref().to_string())
                .collect::<Vec<_>>(),
        )
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
    fn replicate_into<T>(&self) -> Result<T, FrameworkError>
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
    async fn increment(&self, column: &str, by: i64) -> Result<(), FrameworkError> {
        let table = Self::TABLE;
        let pk_name = Self::primary_key_name();
        let pk_value = self.primary_key_value_json();
        let sql = format!("UPDATE {table} SET {column} = {column} + ? WHERE {pk_name} = ?");
        let db = DB::connection()?;
        db.inner()
            .execute(sea_orm::Statement::from_sql_and_values(
                db.inner().get_database_backend(),
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
    fn primary_key_value(&self) -> <<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType;

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
    Self: From<<Self::Entity as EntityTrait>::Model>
        + crate::eloquent::EagerLoadDispatch,
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
