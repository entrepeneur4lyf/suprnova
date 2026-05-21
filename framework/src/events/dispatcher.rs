//! In-process event dispatcher + user-facing `Event` facade.

use super::{ErasedListener, Listener, ListenerWrap};
use crate::FrameworkError;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{debug, error};

/// In-process event dispatcher. Held as a process-global via
/// `OnceLock` in this module; the `Event` facade is the user-facing
/// entry point.
///
/// The inner [`RwLock`] is the std synchronous form (not
/// [`tokio::sync::RwLock`]). Holding the lock across an `.await` is
/// already impossible in the current API — every callsite reads or
/// writes, drops the guard, then awaits separately — so the cheaper
/// std lock buys us a synchronous [`Self::clear`] that can run from
/// `TestContainerGuard`'s `Drop` for test isolation parity with
/// [`crate::database::ConnectionRegistry::clear`].
pub struct EventDispatcher {
    listeners: RwLock<HashMap<TypeId, Vec<Arc<dyn ErasedListener>>>>,
}

impl EventDispatcher {
    pub fn new() -> Self {
        Self {
            listeners: RwLock::new(HashMap::new()),
        }
    }

    /// Register a listener for events of type `E`.
    pub async fn listen<E, L>(&self, listener: Arc<L>)
    where
        E: super::Event,
        L: Listener<E>,
    {
        let wrap = Arc::new(ListenerWrap::<E, L>::new(listener)) as Arc<dyn ErasedListener>;
        self.listeners
            .write()
            .expect("event listener registry poisoned")
            .entry(TypeId::of::<E>())
            .or_default()
            .push(wrap);
    }

    /// Phase 10C audit-fix AF4 — drop every registered listener.
    /// `#[doc(hidden)]` because it's a test-only escape hatch; called
    /// from [`crate::testing::TestContainerGuard::drop`] so the next
    /// test in the same process starts with an empty listener table.
    /// Production code should never call this.
    #[doc(hidden)]
    pub fn clear(&self) {
        if let Ok(mut map) = self.listeners.write() {
            map.clear();
        }
    }

    /// Sync, fallible clear of the process-global dispatcher.
    /// Called by [`crate::testing::TestContainerGuard::drop`].
    #[doc(hidden)]
    pub fn clear_global() {
        if let Some(d) = GLOBAL.get() {
            d.clear();
        }
    }

    /// Dispatch an event. Synchronous events run inline (sequentially,
    /// in registration order). Queued events spawn a tokio task per
    /// listener; this call returns after spawning, not after they
    /// complete.
    pub async fn dispatch<E: super::Event>(&self, event: E) -> Result<(), FrameworkError> {
        let listeners = {
            let map = self
                .listeners
                .read()
                .expect("event listener registry poisoned");
            map.get(&TypeId::of::<E>()).cloned().unwrap_or_default()
        };

        debug!(
            event = E::event_name(),
            listeners = listeners.len(),
            queued = E::queued(),
            "dispatching event"
        );

        if E::queued() {
            for l in listeners {
                let event_clone = event.clone();
                tokio::spawn(async move {
                    if let Err(e) = l.dispatch(&event_clone).await {
                        error!(
                            event = E::event_name(),
                            error = %e,
                            "queued listener failed"
                        );
                    }
                });
            }
        } else {
            for l in listeners {
                l.dispatch(&event).await?;
            }
        }

        Ok(())
    }
}

impl Default for EventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Process-global dispatcher.
static GLOBAL: std::sync::OnceLock<EventDispatcher> = std::sync::OnceLock::new();

fn global() -> &'static EventDispatcher {
    GLOBAL.get_or_init(EventDispatcher::new)
}

/// User-facing facade. Routes through the global dispatcher (or the
/// fake recorder if `Event::fake()` is active).
pub struct Event;

impl Event {
    /// Dispatch an event. Routes through the fake recorder if
    /// `Event::fake()` is active (recording the event without invoking
    /// listeners); otherwise delegates to the global `EventDispatcher`.
    pub async fn dispatch<E: super::Event>(event: E) -> Result<(), FrameworkError> {
        if super::testing::is_active() {
            super::testing::record(event);
            return Ok(());
        }
        global().dispatch(event).await
    }

    /// Register a listener for events of type `E`.
    pub async fn listen<E, L>(listener: Arc<L>)
    where
        E: super::Event,
        L: Listener<E>,
    {
        global().listen(listener).await;
    }

    /// Register a `BroadcastListener<E>` so dispatching events of
    /// type `E` also publishes them to the hub channels named by
    /// `E::broadcast_on()`. Call once per Broadcastable type at
    /// boot.
    pub async fn broadcast<E: crate::broadcasting::Broadcastable>(
        hub: std::sync::Arc<dyn crate::broadcasting::BroadcastHub>,
    ) {
        Self::listen::<E, crate::broadcasting::BroadcastListener<E>>(std::sync::Arc::new(
            crate::broadcasting::BroadcastListener::<E>::new(hub),
        ))
        .await;
    }

    /// Replace the global dispatcher with a fake. Returns a guard
    /// that restores listener invocation on drop. Available to
    /// consumer-crate tests by default — no feature gate.
    pub fn fake() -> super::testing::EventFakeGuard {
        super::testing::install_fake()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event as EventTrait, Listener};
    use crate::FrameworkError;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    struct Pinged {
        pub n: i64,
    }
    impl EventTrait for Pinged {
        fn event_name() -> &'static str {
            "Pinged"
        }
    }

    struct Counter(Arc<AtomicI64>);
    #[async_trait]
    impl Listener<Pinged> for Counter {
        async fn handle(&self, event: &Pinged) -> Result<(), FrameworkError> {
            self.0.fetch_add(event.n, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn dispatch_calls_registered_listener() {
        let d = EventDispatcher::new();
        let count = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged, _>(Arc::new(Counter(count.clone()))).await;
        d.dispatch(Pinged { n: 5 }).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn dispatch_with_no_listeners_is_ok() {
        let d = EventDispatcher::new();
        d.dispatch(Pinged { n: 1 }).await.unwrap();
    }

    #[tokio::test]
    async fn dispatch_calls_all_listeners() {
        let d = EventDispatcher::new();
        let a = Arc::new(AtomicI64::new(0));
        let b = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged, _>(Arc::new(Counter(a.clone()))).await;
        d.listen::<Pinged, _>(Arc::new(Counter(b.clone()))).await;
        d.dispatch(Pinged { n: 3 }).await.unwrap();
        assert_eq!(a.load(Ordering::SeqCst), 3);
        assert_eq!(b.load(Ordering::SeqCst), 3);
    }

    #[derive(Debug, Clone)]
    struct QueuedPing;
    impl EventTrait for QueuedPing {
        fn event_name() -> &'static str {
            "QueuedPing"
        }
        fn queued() -> bool {
            true
        }
    }

    struct SlowCounter(Arc<AtomicI64>);
    #[async_trait]
    impl Listener<QueuedPing> for SlowCounter {
        async fn handle(&self, _event: &QueuedPing) -> Result<(), FrameworkError> {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn queued_event_returns_before_listener_completes() {
        let d = EventDispatcher::new();
        let n = Arc::new(AtomicI64::new(0));
        d.listen::<QueuedPing, _>(Arc::new(SlowCounter(n.clone()))).await;
        d.dispatch(QueuedPing).await.unwrap();
        // Immediately after dispatch returns, the slow listener has
        // not had time to complete (it sleeps 20ms).
        assert_eq!(n.load(Ordering::SeqCst), 0);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }
}
