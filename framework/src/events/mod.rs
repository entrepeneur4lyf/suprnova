//! Typed in-process pub/sub.
//!
//! ```ignore
//! use suprnova::{Event, EventFacade, Listener, FrameworkError};
//! use std::sync::Arc;
//!
//! #[derive(Debug, Clone)]
//! pub struct UserRegistered { pub user_id: i64 }
//!
//! impl Event for UserRegistered {
//!     fn event_name() -> &'static str { "UserRegistered" }
//! }
//!
//! pub struct SendWelcomeEmail;
//!
//! #[suprnova::async_trait]
//! impl Listener<UserRegistered> for SendWelcomeEmail {
//!     async fn handle(&self, event: &UserRegistered) -> Result<(), FrameworkError> {
//!         // ...
//!         Ok(())
//!     }
//! }
//!
//! // In bootstrap.rs:
//! EventFacade::listen::<UserRegistered>(Arc::new(SendWelcomeEmail)).await;
//!
//! // In a controller:
//! EventFacade::dispatch(UserRegistered { user_id: 42 }).await?;
//! ```

mod builtins;
mod dispatcher;
mod queued_listener;
pub mod testing;

pub use builtins::ErrorOccurred;
pub use dispatcher::{Event as EventFacade, EventDispatcher};
pub use queued_listener::QueuedListener;
pub use testing::{
    EventFakeGuard, assert_dispatched, assert_dispatched_once, assert_dispatched_times,
    assert_listening, assert_not_dispatched, assert_nothing_dispatched, dispatched,
    dispatched_count, dispatched_events, has_dispatched,
};

use crate::FrameworkError;
use async_trait::async_trait;
use std::any::Any;
use std::sync::Arc;

/// A typed event payload.
///
/// `Send + Sync + Clone + 'static` so it can cross task boundaries
/// for queued listeners; `Debug` so the dispatcher can log it.
pub trait Event: Send + Sync + Clone + 'static + std::fmt::Debug {
    /// Stable name used for logging and fake assertions.
    fn event_name() -> &'static str
    where
        Self: Sized;

    /// Whether this event should be delivered asynchronously
    /// (spawned task) or synchronously (inline). Default: sync.
    fn queued() -> bool
    where
        Self: Sized,
    {
        false
    }
}

/// A listener that handles events of type `E`.
#[async_trait]
pub trait Listener<E: Event>: Send + Sync + 'static {
    async fn handle(&self, event: &E) -> Result<(), FrameworkError>;
}

/// A subscriber bundles a related set of listener registrations behind one
/// bootstrap call. Mirrors Laravel's `EventServiceProvider` subscriber pattern:
/// instead of registering ten listeners individually in `bootstrap.rs`, a
/// single struct implements `Subscriber` and registers them all from its
/// `subscribe` method.
///
/// ```ignore
/// use suprnova::{EventFacade, Subscriber, EventDispatcher};
/// use std::sync::Arc;
///
/// pub struct UserEventSubscriber;
///
/// #[suprnova::async_trait]
/// impl Subscriber for UserEventSubscriber {
///     async fn subscribe(self: Arc<Self>, d: &EventDispatcher) {
///         d.listen::<UserRegistered, _>(Arc::new(SendWelcomeEmail)).await;
///         d.listen::<UserDeleted, _>(Arc::new(CleanupUserData)).await;
///     }
/// }
///
/// // In bootstrap.rs:
/// EventFacade::subscribe(Arc::new(UserEventSubscriber)).await;
/// ```
#[async_trait]
pub trait Subscriber: Send + Sync + 'static {
    /// Attach every listener this subscriber owns to the dispatcher.
    /// `self` is `Arc<Self>` so listeners that need to share state with the
    /// subscriber can clone the `Arc` and capture it.
    async fn subscribe(self: Arc<Self>, dispatcher: &EventDispatcher);
}

/// Trait-object compatible bridge between concrete listeners and the
/// dispatcher's `Vec<Arc<dyn ErasedListener>>` storage.
#[async_trait]
pub(crate) trait ErasedListener: Send + Sync {
    async fn dispatch(&self, event: &(dyn Any + Send + Sync)) -> Result<(), FrameworkError>;
}

pub(crate) struct ListenerWrap<E: Event, L: Listener<E>> {
    listener: Arc<L>,
    _marker: std::marker::PhantomData<E>,
}

impl<E: Event, L: Listener<E>> ListenerWrap<E, L> {
    pub fn new(listener: Arc<L>) -> Self {
        Self {
            listener,
            _marker: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<E, L> ErasedListener for ListenerWrap<E, L>
where
    E: Event,
    L: Listener<E>,
{
    async fn dispatch(&self, event: &(dyn Any + Send + Sync)) -> Result<(), FrameworkError> {
        match event.downcast_ref::<E>() {
            Some(typed) => self.listener.handle(typed).await,
            None => {
                tracing::error!(
                    event_type = std::any::type_name::<E>(),
                    "event dispatcher routed an event to a listener whose typed \
                     payload does not match; skipping invocation rather than \
                     panicking. This indicates TypeId-keying corruption in the \
                     dispatcher's listener map."
                );
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    struct OrderPlaced {
        #[allow(dead_code)]
        pub order_id: i64,
    }

    impl Event for OrderPlaced {
        fn event_name() -> &'static str {
            "OrderPlaced"
        }
    }

    #[test]
    fn event_name_is_static_str() {
        assert_eq!(OrderPlaced::event_name(), "OrderPlaced");
    }

    #[test]
    fn event_default_queued_is_false() {
        assert!(!OrderPlaced::queued());
    }

    // A no-op listener used to construct an `ErasedListener` for the
    // type-mismatch regression test below.
    struct NoopListener;

    #[async_trait]
    impl Listener<OrderPlaced> for NoopListener {
        async fn handle(&self, _event: &OrderPlaced) -> Result<(), FrameworkError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn erased_listener_returns_ok_on_type_mismatch_instead_of_panicking() {
        // If the dispatcher's TypeId keying ever routes a wrong-typed
        // payload to a listener, the erased wrapper must log and degrade
        // to a no-op rather than panic the dispatch task. Constructing
        // the wrap directly + feeding it a non-OrderPlaced `&dyn Any`
        // exercises the downcast-failure arm exactly.
        let wrap: Box<dyn ErasedListener> =
            Box::new(ListenerWrap::<OrderPlaced, _>::new(Arc::new(NoopListener)));
        let wrong_payload: i32 = 42;
        let result = wrap.dispatch(&wrong_payload).await;
        assert!(result.is_ok(), "expected Ok on type mismatch, got {result:?}");
    }
}
