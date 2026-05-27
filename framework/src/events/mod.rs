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
        let typed = event
            .downcast_ref::<E>()
            .expect("dispatcher routed event to wrong listener type");
        self.listener.handle(typed).await
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
}
