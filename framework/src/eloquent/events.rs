//! Phase 10C T1 — Model lifecycle events shared types.
//!
//! Sixteen lifecycle events fire on every `#[suprnova::model]` struct.
//! The per-type event structs are macro-emitted into each model's
//! `events::` submodule (`user::events::Created`,
//! `user::events::Saving`, ...). This file holds the cross-model
//! types that don't depend on the concrete model type:
//!
//! - [`EventResult`] — `Ok` / `Cancel(reason)` returned by listeners
//!   on the five cancellable events
//!   (`Saving`/`Creating`/`Updating`/`Deleting`/`Restoring`).
//! - [`CancellableListener`] — listener trait for those events.
//!   Distinct from the regular [`crate::events::Listener`] because
//!   cancel-by-policy and cancel-by-error have different shapes.
//! - [`dispatch_cancellable`] / [`dispatch_after`] — dispatch helpers
//!   the macro-emitted [`ModelEventHooks`] impl calls into.
//! - [`ModelEventHooks`] — the bridge trait: every `#[suprnova::model]`
//!   struct receives a macro-generated impl that wires its CRUD
//!   methods to the per-type event structs above.
//!
//! Cancel signals propagate as
//! [`FrameworkError::bad_request`](crate::FrameworkError::bad_request)
//! to the caller — `delete()` / `create()` / `save()` etc. return
//! the bad-request error with the listener's reason verbatim. We do
//! NOT introduce a new `FrameworkError::Cancelled` variant: the public
//! error surface stays narrow, and "policy refused this operation"
//! maps cleanly to HTTP 400 already.

use crate::FrameworkError;
use crate::eloquent::attrs::Attrs;
use crate::events::{Event, EventFacade};
use async_trait::async_trait;
use std::sync::Arc;

/// Result returned by listeners on cancellable lifecycle events
/// (`Saving` / `Creating` / `Updating` / `Deleting` / `Restoring`).
///
/// Matches Laravel's `return false` cancel semantics without
/// overloading return values: listeners that succeed return
/// `EventResult::ok()`; listeners that veto return
/// `EventResult::cancel(reason)`. The reason string surfaces to the
/// caller through
/// [`FrameworkError::bad_request`](crate::FrameworkError::bad_request).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventResult {
    /// Continue the operation.
    Ok,
    /// Abort the operation with the given reason. Surfaces as
    /// `FrameworkError::bad_request(reason)` to the caller.
    Cancel(String),
}

impl EventResult {
    /// Continue the operation.
    pub fn ok() -> Self {
        Self::Ok
    }

    /// Abort the operation with the given reason.
    pub fn cancel(reason: impl Into<String>) -> Self {
        Self::Cancel(reason.into())
    }

    /// Whether this result vetoes the operation.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancel(_))
    }
}

/// Listener for cancellable lifecycle events. Returns `EventResult`
/// instead of `Result<(), FrameworkError>` because cancel-by-policy
/// is a different shape from cancel-by-error.
///
/// Register via [`listen_cancellable`].
#[async_trait]
pub trait CancellableListener<E: Event>: Send + Sync + 'static {
    async fn handle(&self, event: &E) -> EventResult;
}

/// Trait-object compatible bridge between concrete
/// [`CancellableListener`]s and the registry's heterogeneous
/// `Vec<Arc<dyn ErasedCancellableListener>>` storage. Mirrors the
/// existing `ErasedListener` pattern in [`crate::events`].
#[async_trait]
pub(crate) trait ErasedCancellableListener: Send + Sync {
    async fn dispatch(&self, event: &(dyn std::any::Any + Send + Sync)) -> EventResult;
}

pub(crate) struct CancellableListenerWrap<E: Event, L: CancellableListener<E>> {
    listener: Arc<L>,
    _marker: std::marker::PhantomData<E>,
}

impl<E: Event, L: CancellableListener<E>> CancellableListenerWrap<E, L> {
    pub fn new(listener: Arc<L>) -> Self {
        Self {
            listener,
            _marker: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<E, L> ErasedCancellableListener for CancellableListenerWrap<E, L>
where
    E: Event,
    L: CancellableListener<E>,
{
    async fn dispatch(&self, event: &(dyn std::any::Any + Send + Sync)) -> EventResult {
        let typed = event
            .downcast_ref::<E>()
            .expect("dispatcher routed cancellable event to wrong listener type");
        self.listener.handle(typed).await
    }
}

/// Dispatch a cancellable event to every registered listener.
///
/// - Returns `Ok(())` when every listener returned `EventResult::Ok`
///   (or no listeners are registered).
/// - Returns `Err(FrameworkError::bad_request(reason))` at the FIRST
///   listener that returned `EventResult::Cancel(reason)`. Later
///   listeners are NOT called — the operation is already vetoed.
///
/// The macro-emitted [`ModelEventHooks`] impl on each model calls
/// this from `__dispatch_creating` / `__dispatch_saving` /
/// `__dispatch_updating` / `__dispatch_deleting` /
/// `__dispatch_restoring`.
pub async fn dispatch_cancellable<E: Event + Clone>(event: E) -> Result<(), FrameworkError> {
    let listeners = global_cancellable_listeners::<E>().await;
    if listeners.is_empty() {
        return Ok(());
    }
    for l in listeners {
        // Each listener gets its own clone — the same `event` value
        // is reused across all of them when no listener cancels.
        let event_any: Box<dyn std::any::Any + Send + Sync> = Box::new(event.clone());
        match l.dispatch(&*event_any).await {
            EventResult::Ok => continue,
            EventResult::Cancel(reason) => {
                return Err(FrameworkError::bad_request(reason));
            }
        }
    }
    Ok(())
}

/// Fire a non-cancellable event through the existing
/// [`crate::events::EventFacade`]. Thin wrapper so the
/// macro-emitted dispatch sites read consistently with the
/// cancellable counterpart.
pub async fn dispatch_after<E: Event>(event: E) -> Result<(), FrameworkError> {
    EventFacade::dispatch(event).await
}

// --- Cancellable listener registry --------------------------------------

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Default)]
struct CancellableRegistry {
    listeners: HashMap<TypeId, Vec<Arc<dyn ErasedCancellableListener>>>,
}

// Phase 10C audit-fix AF4 — std::sync::RwLock so the AF4 clear() helper
// runs from sync teardown (`TestContainerGuard::drop`). Callers never
// hold this lock across an `.await`, so the sync flavour is safe.
static CANCELLABLE_REGISTRY: std::sync::OnceLock<RwLock<CancellableRegistry>> =
    std::sync::OnceLock::new();

fn registry() -> &'static RwLock<CancellableRegistry> {
    CANCELLABLE_REGISTRY.get_or_init(|| RwLock::new(CancellableRegistry::default()))
}

/// Register a [`CancellableListener`] for events of type `E`. Mirrors
/// `EventFacade::listen` for the cancellable surface.
///
/// Phase 10C T2 (observers) wraps this with attribute-driven
/// auto-registration; T1 exposes the manual entry point.
///
/// **Poison policy** (Domain 9 audit D9-A): if the global registry's
/// `RwLock` is poisoned (a previous writer panicked while holding the
/// guard), the registration is skipped after a `tracing::error!`.
/// Production: an app whose registry is poisoned has bigger problems
/// than a missing listener — the log lets ops surface that.
pub async fn listen_cancellable<E: Event, L: CancellableListener<E>>(listener: Arc<L>) {
    match registry().write() {
        Ok(mut reg) => {
            reg.listeners
                .entry(TypeId::of::<E>())
                .or_default()
                .push(Arc::new(CancellableListenerWrap::<E, L>::new(listener)));
        }
        Err(_) => {
            tracing::error!(
                event_type = std::any::type_name::<E>(),
                "cancellable listener registry lock poisoned; \
                 skipping registration. A prior writer panicked under \
                 the write guard — the framework can no longer dispatch \
                 cancellable events for this type."
            );
        }
    }
}

async fn global_cancellable_listeners<E: Event>() -> Vec<Arc<dyn ErasedCancellableListener>> {
    // Domain 9 audit D9-A — degrade to empty vec on poison rather than
    // panicking the dispatcher path. Empty == "no listeners registered",
    // which is the safe fallback: dispatch proceeds with no
    // cancellation possible (the operation is allowed by default per
    // event semantics).
    match registry().read() {
        Ok(reg) => reg
            .listeners
            .get(&TypeId::of::<E>())
            .cloned()
            .unwrap_or_default(),
        Err(_) => {
            tracing::error!(
                event_type = std::any::type_name::<E>(),
                "cancellable listener registry lock poisoned during dispatch; \
                 treating as empty (no listeners), event proceeds uncancelled.",
            );
            Vec::new()
        }
    }
}

/// Phase 10C audit-fix AF4 — wipe every registered cancellable
/// listener. Sync + `#[doc(hidden)]` so it can run from
/// `TestContainerGuard::drop` for test isolation parity with
/// [`crate::database::ConnectionRegistry::clear`]. Production code
/// should never call this — listener registration is process-lifetime
/// in real apps.
#[doc(hidden)]
pub fn clear_cancellable_listeners() {
    if let Some(lock) = CANCELLABLE_REGISTRY.get()
        && let Ok(mut reg) = lock.write()
    {
        reg.listeners.clear();
    }
}

/// Macro-implemented hooks bridging each [`Model`](crate::eloquent::Model)
/// to its per-type [`events`](crate::eloquent::events) submodule.
///
/// Users never implement this — every `#[suprnova::model]` struct
/// receives one impl from the macro. The `Model` trait's `create` /
/// `save` / `update` / `delete` / `force_delete` and related
/// soft-delete paths call into these hooks at the right lifecycle
/// points; the macro fills them in with calls to the
/// per-model `events::*` structs.
///
/// The trait sits parallel to (not nested inside) `Model` so the
/// macro can ship a single, self-contained impl per model without
/// having to thread through the broader `Model` where-clause.
#[async_trait]
pub trait ModelEventHooks: Sized {
    async fn __dispatch_creating(
        attrs: Arc<tokio::sync::Mutex<Attrs>>,
    ) -> Result<(), FrameworkError>;
    async fn __dispatch_saving(
        attrs: Arc<tokio::sync::Mutex<Attrs>>,
        is_creating: bool,
    ) -> Result<(), FrameworkError>;
    async fn __dispatch_created(model: &Self) -> Result<(), FrameworkError>;
    async fn __dispatch_saved(model: &Self) -> Result<(), FrameworkError>;
    async fn __dispatch_updating(
        previous: &Self,
        attrs: Arc<tokio::sync::Mutex<Attrs>>,
    ) -> Result<(), FrameworkError>;
    async fn __dispatch_updated(previous: &Self, current: &Self) -> Result<(), FrameworkError>;
    async fn __dispatch_deleting(model: &Self, is_force: bool) -> Result<(), FrameworkError>;
    async fn __dispatch_deleted(model: &Self, is_force: bool) -> Result<(), FrameworkError>;
    async fn __dispatch_trashed(model: &Self) -> Result<(), FrameworkError>;
    async fn __dispatch_restoring(model: &Self) -> Result<(), FrameworkError>;
    async fn __dispatch_restored(model: &Self) -> Result<(), FrameworkError>;
    async fn __dispatch_force_deleting(model: &Self) -> Result<(), FrameworkError>;
    async fn __dispatch_force_deleted(model: &Self) -> Result<(), FrameworkError>;
    async fn __dispatch_replicating(
        source: &Self,
        replica: Arc<tokio::sync::Mutex<Self>>,
    ) -> Result<(), FrameworkError>;
    async fn __dispatch_retrieving() -> Result<(), FrameworkError>;
    async fn __dispatch_retrieved(model: &Self) -> Result<(), FrameworkError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_is_not_cancelled() {
        assert!(!EventResult::ok().is_cancelled());
    }

    #[test]
    fn cancel_carries_reason() {
        assert!(EventResult::cancel("nope").is_cancelled());
        match EventResult::cancel("nope") {
            EventResult::Cancel(r) => assert_eq!(r, "nope"),
            _ => panic!("expected cancel"),
        }
    }
}
