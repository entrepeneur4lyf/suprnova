//! Phase 10C T2a — `Observer<M>` trait + auto-registration inventory.
//!
//! Observers are typed listeners that collect a model's 16 lifecycle
//! callbacks into a single trait impl. A user writes:
//!
//! ```rust,ignore
//! pub struct UserObserver;
//!
//! #[suprnova::observer(User)]    // <- T2b ships this macro.
//! impl Observer<User> for UserObserver {
//!     async fn creating(&self, attrs: &mut Attrs) -> EventResult {
//!         if attrs.get("email").is_none() {
//!             return EventResult::cancel("email is required");
//!         }
//!         EventResult::ok()
//!     }
//!
//!     async fn created(&self, user: &User) -> Result<(), FrameworkError> {
//!         tracing::info!(user.id = user.id, "user created");
//!         Ok(())
//!     }
//! }
//! ```
//!
//! and gets two things for free:
//!
//! 1. The user only writes the methods they care about. Every method
//!    has a default no-op, so an impl block with zero method bodies
//!    is legal and registers no listeners.
//! 2. The macro walks the impl block at parse time, identifies which
//!    methods diverge from the trait default by NAME (the trait's
//!    method names are the closed set of 16 below), and emits one
//!    listener-registration call per non-default method. At startup,
//!    [`bootstrap_observers`] drains the inventory and calls each
//!    registered observer's install closure once.
//!
//! T2a (this file) ships:
//! - The [`Observer<M>`] trait with 16 default no-op methods.
//! - [`ObserverEntry`] — the inventory entry submitted by T2b's
//!   macro.
//! - [`ObserverInstallFuture`] — type alias for the boxed install
//!   future, kept separate so `ObserverEntry::install` reads cleanly.
//! - [`bootstrap_observers`] — drains the inventory once at boot.
//!
//! T2b will ship the `#[suprnova::observer(M)]` attribute macro that
//! emits `inventory::submit!{ObserverEntry { ... }}` entries.
//! T2c will ship `Model::observe()` manual registration + the
//! `#[model(observers = [...])]` validation pass + the multi-observer
//! and cancel-from-observer tests.

use crate::FrameworkError;
use crate::eloquent::attrs::Attrs;
use crate::eloquent::events::EventResult;
use crate::eloquent::model::Model;
use async_trait::async_trait;

/// Lifecycle observer for model `M`.
///
/// Sixteen methods, one per lifecycle event T1 ships:
///
/// **Cancellable (return [`EventResult`])** — `Cancel(reason)` aborts
/// the in-flight operation with
/// [`FrameworkError::bad_request(reason)`](crate::FrameworkError::bad_request):
///
/// - [`saving`](Self::saving) — fires before both `create` and `save`
///   with `is_creating: bool` to disambiguate.
/// - [`creating`](Self::creating) — fires before `create`.
/// - [`updating`](Self::updating) — fires before `update` / `save` on
///   an existing row. Carries the pre-update snapshot.
/// - [`deleting`](Self::deleting) — fires before `delete` (soft or
///   hard). `is_force` is `true` for `force_delete` on a soft-delete
///   model.
/// - [`restoring`](Self::restoring) — fires before `restore` on a
///   soft-delete model.
///
/// **Non-cancellable (return `Result<(), FrameworkError>`)** — listener
/// errors propagate but cannot stop the operation since it has already
/// happened (or, in the case of `retrieving`, has been initiated):
///
/// - [`retrieving`](Self::retrieving), [`retrieved`](Self::retrieved)
///   — Builder query lifecycle (T1's `Builder::get` / `first` /
///   `first_or_fail` paths).
/// - [`created`](Self::created), [`updated`](Self::updated),
///   [`saved`](Self::saved) — fired after the corresponding cancellable
///   counterpart succeeds.
/// - [`deleted`](Self::deleted) — fired after `delete`.
///   `is_force` matches the `Deleting` event flag.
/// - [`trashed`](Self::trashed) — fired ONLY after a soft-delete (not
///   `force_delete`).
/// - [`restored`](Self::restored) — fired after `restore`.
/// - [`replicating`](Self::replicating) — fired during `replicate` /
///   `replicate_except` / `replicate_into` BEFORE the replica is
///   returned. Takes `&mut M` so listeners may clear timestamps,
///   reset auto-increments, etc.
/// - [`force_deleting`](Self::force_deleting),
///   [`force_deleted`](Self::force_deleted) — fired before/after a
///   `force_delete` on a soft-delete model.
///
/// All defaults are no-ops, so a user implementing the trait writes
/// ONLY the methods they care about:
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use suprnova::eloquent::observers::Observer;
/// use suprnova::eloquent::attrs::Attrs;
/// use suprnova::eloquent::events::EventResult;
///
/// pub struct AuditObserver;
///
/// #[async_trait]
/// impl Observer<User> for AuditObserver {
///     async fn created(&self, user: &User) -> Result<(), suprnova::FrameworkError> {
///         tracing::info!(user.id = user.id, "user created");
///         Ok(())
///     }
/// }
/// ```
///
/// The `M: Model + 'static` bound is what lets T2b's macro emit
/// per-method adapter listeners that reference `M::events::Saving`,
/// `M::events::Created`, etc. without a generic shim.
///
/// The where-clause re-elaborates `Model`'s own bounds because Rust's
/// trait elaboration doesn't transitively propagate associated-type
/// bounds from a supertrait's where-clause to a subtrait's. Without
/// these, downstream impls and the trait's own method bodies fail to
/// type-check against the same constraints `Model::query()` uses. Same
/// pattern as `Builder<M>` in `eloquent/builder.rs`.
#[async_trait]
pub trait Observer<M>: Send + Sync + 'static
where
    M: Model + 'static,
    M: From<<M::Entity as sea_orm::EntityTrait>::Model>,
    <M::Entity as sea_orm::EntityTrait>::Model: From<M>
        + sea_orm::IntoActiveModel<<M::Entity as sea_orm::EntityTrait>::ActiveModel>
        + serde::Serialize
        + Send
        + Sync,
    <M::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<M::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    // -- Builder lifecycle ----------------------------------------------

    /// Fires once before a Builder query touches the database.
    /// Listener errors propagate; the operation has not yet started.
    async fn retrieving(&self) -> Result<(), FrameworkError> {
        Ok(())
    }

    /// Fires once per row returned by a Builder query.
    async fn retrieved(&self, _model: &M) -> Result<(), FrameworkError> {
        Ok(())
    }

    // -- Cancellable write lifecycle ------------------------------------

    /// Fires before both `create` and `save`. Cancellable.
    /// `is_creating` is `true` on the insert path and `false` on the
    /// update path — a single listener may branch on which is firing.
    async fn saving(&self, _attrs: &mut Attrs, _is_creating: bool) -> EventResult {
        EventResult::ok()
    }

    /// Fires before `create`. Cancellable. The `attrs` map carries the
    /// in-flight column values; a listener may mutate them in place
    /// before the INSERT lands.
    async fn creating(&self, _attrs: &mut Attrs) -> EventResult {
        EventResult::ok()
    }

    /// Fires before `update` / `save` on an existing row. Cancellable.
    /// `previous` is the pre-update model snapshot; `attrs` is the
    /// in-flight mutation map.
    async fn updating(&self, _previous: &M, _attrs: &mut Attrs) -> EventResult {
        EventResult::ok()
    }

    /// Fires before `delete` (soft or hard). Cancellable.
    /// `is_force` is `true` when invoked via `force_delete` on a
    /// soft-delete model.
    async fn deleting(&self, _model: &M, _is_force: bool) -> EventResult {
        EventResult::ok()
    }

    /// Fires before `restore` on a soft-delete model. Cancellable —
    /// a listener may refuse the un-tombstone operation.
    async fn restoring(&self, _model: &M) -> EventResult {
        EventResult::ok()
    }

    // -- Non-cancellable after-the-fact -----------------------------------

    /// Fires after a successful `create`.
    async fn created(&self, _model: &M) -> Result<(), FrameworkError> {
        Ok(())
    }

    /// Fires after a successful `update` / `save`. `previous` is the
    /// pre-update snapshot, `current` is the row as it now exists.
    async fn updated(&self, _previous: &M, _current: &M) -> Result<(), FrameworkError> {
        Ok(())
    }

    /// Fires after both `create` and `save` succeed.
    async fn saved(&self, _model: &M) -> Result<(), FrameworkError> {
        Ok(())
    }

    /// Fires after a successful `delete`. `is_force` matches the
    /// `Deleting` event flag — `true` for `force_delete` on a
    /// soft-delete model, `false` otherwise.
    async fn deleted(&self, _model: &M, _is_force: bool) -> Result<(), FrameworkError> {
        Ok(())
    }

    /// Fires ONLY after a soft-delete on a model with
    /// `#[model(soft_deletes)]`. NOT fired by `force_delete` (which
    /// removes the row outright).
    async fn trashed(&self, _model: &M) -> Result<(), FrameworkError> {
        Ok(())
    }

    /// Fires after a successful `restore`.
    async fn restored(&self, _model: &M) -> Result<(), FrameworkError> {
        Ok(())
    }

    /// Fires during `replicate` / `replicate_except` / `replicate_into`
    /// BEFORE the replica is returned. `source` is the original;
    /// `replica` is the freshly built clone — listeners can mutate it
    /// in place (clear timestamps, reset flags, regenerate UUIDs, ...).
    async fn replicating(&self, _source: &M, _replica: &mut M) -> Result<(), FrameworkError> {
        Ok(())
    }

    /// Fires before `force_delete` on a soft-delete model.
    /// Non-cancellable — the cancellable hook for force_delete fires
    /// as `Deleting { is_force: true }`. This event is the explicit
    /// after-the-fact pair for users who want to discriminate the
    /// hard-delete code path.
    async fn force_deleting(&self, _model: &M) -> Result<(), FrameworkError> {
        Ok(())
    }

    /// Fires after a successful `force_delete` on a soft-delete model.
    async fn force_deleted(&self, _model: &M) -> Result<(), FrameworkError> {
        Ok(())
    }
}

/// Boxed install future used by [`ObserverEntry::install`].
///
/// Kept as a separate type alias so the `ObserverEntry::install`
/// function-pointer signature stays readable. T2b's macro emits
/// install closures shaped like
/// `|| Box::pin(async move { ... register listeners ... })`.
pub type ObserverInstallFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(), FrameworkError>> + Send + 'static>,
>;

/// Compile-time inventory entry submitted by T2b's
/// `#[suprnova::observer(M)]` attribute macro.
///
/// Each `#[observer(User)]` invocation emits one
/// `inventory::submit!{ObserverEntry { ... }}` block. At boot,
/// [`bootstrap_observers`] iterates `inventory::iter::<ObserverEntry>`
/// and `await`s each entry's `install` closure. The install closure
/// is responsible for calling
/// [`EventFacade::listen`](crate::events::EventFacade::listen) and
/// [`listen_cancellable`](crate::eloquent::events::listen_cancellable)
/// for each method the user actually overrode in the impl block.
///
/// `name` is the Rust type name of the observer struct (e.g.
/// `"UserObserver"`). It's surfaced in `tracing` spans so install
/// failures point at the right type.
pub struct ObserverEntry {
    /// The observer struct's type name (e.g. `"UserObserver"`).
    pub name: &'static str,
    /// Closure that registers the observer's per-method listeners
    /// with the framework's event registries. Returns
    /// [`ObserverInstallFuture`] so registration can be async (the
    /// underlying `listen` / `listen_cancellable` calls take a
    /// `RwLock::write().await`).
    pub install: fn() -> ObserverInstallFuture,
}

inventory::collect!(ObserverEntry);

/// Drain the inventory and install every registered observer.
///
/// Called once at startup (by `App::bootstrap`, T2c will wire that
/// integration; T2a only ships the entry point). For each
/// `ObserverEntry` in `inventory::iter::<ObserverEntry>`, this
/// awaits the entry's `install` closure.
///
/// **Idempotency is T2b's contract, not T2a's.** This function
/// unconditionally calls every entry's install closure every time
/// it runs. Calling it twice WILL double-install whatever those
/// closures do. T2b's `#[suprnova::observer(M)]` macro is
/// responsible for emitting install closures that guard against
/// double-registration (`EventFacade::listen` is append-only, so
/// the macro must use a `OnceLock`/`Once` gate per observer type).
/// T2a only guarantees that an EMPTY inventory bootstraps cleanly
/// — which is all that's required for T2a itself, since the macro
/// that submits entries doesn't ship until T2b.
///
/// Returns the first install error encountered. If one observer's
/// install fails, later observers are NOT installed — the boot is
/// already broken.
pub async fn bootstrap_observers() -> Result<(), FrameworkError> {
    for entry in inventory::iter::<ObserverEntry> {
        (entry.install)().await.map_err(|e| {
            FrameworkError::internal(format!("observer install failed for {}: {e}", entry.name))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Local smoke test — kept inline so the trait + bootstrap surface
    // is exercised even without the integration test file (which lives
    // at `framework/tests/eloquent_observers.rs` for the cross-crate
    // re-export check).
    #[tokio::test]
    async fn bootstrap_empty_inventory_succeeds() {
        bootstrap_observers()
            .await
            .expect("empty inventory should bootstrap cleanly");
    }
}
