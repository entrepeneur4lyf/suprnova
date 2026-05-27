//! In-process event dispatcher + user-facing `Event` facade.

use super::{ErasedListener, Listener, ListenerWrap};
use crate::FrameworkError;
use rand::RngExt;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{Mutex as TokioMutex, Semaphore};
use tokio::task::JoinSet;
use tracing::{debug, error, warn};

/// Default ceiling on concurrently-running queued listener tasks. Overridable
/// per dispatcher via [`EventDispatcher::with_concurrency`] or, for the global
/// dispatcher, the `EVENT_MAX_CONCURRENCY` env var.
const DEFAULT_QUEUED_CONCURRENCY: usize = 256;

/// How many times a queued listener is attempted before the failure is logged
/// and the task gives up. 1 original try + retries. In-process retries are for
/// transient faults (a brief network blip); work that must survive a crash
/// belongs on the durable queue — see [`crate::events`] module docs.
const MAX_QUEUED_ATTEMPTS: u32 = 3;

/// In-process event dispatcher. Held as a process-global via
/// `OnceLock` in this module; the `Event` facade is the user-facing
/// entry point.
///
/// The listener-table [`RwLock`] is the std synchronous form (not
/// [`tokio::sync::RwLock`]). Holding the lock across an `.await` is
/// already impossible in the current API — every callsite reads or
/// writes, drops the guard, then awaits separately — so the cheaper
/// std lock buys us a synchronous [`Self::clear`] that can run from
/// `TestContainerGuard`'s `Drop` for test isolation parity with
/// [`crate::database::ConnectionRegistry::clear`].
///
/// Queued listeners run as spawned tasks tracked in `queued_tasks` so a
/// graceful shutdown can drain them ([`Self::drain_queued`]), bounded by
/// `queued_permits` so an event flood cannot spawn unbounded work. The
/// semaphore is an [`Arc`] so an owned permit can move into the spawned task
/// and release on completion regardless of whether the dispatcher is the
/// `'static` global or a test-local instance.
pub struct EventDispatcher {
    listeners: RwLock<HashMap<TypeId, Vec<Arc<dyn ErasedListener>>>>,
    queued_tasks: TokioMutex<JoinSet<()>>,
    queued_permits: Arc<Semaphore>,
}

impl EventDispatcher {
    pub fn new() -> Self {
        let concurrency = std::env::var("EVENT_MAX_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_QUEUED_CONCURRENCY);
        Self::with_concurrency(concurrency)
    }

    /// Construct a dispatcher with an explicit ceiling on concurrent queued
    /// listener tasks.
    pub fn with_concurrency(queued_concurrency: usize) -> Self {
        Self {
            listeners: RwLock::new(HashMap::new()),
            queued_tasks: TokioMutex::new(JoinSet::new()),
            queued_permits: Arc::new(Semaphore::new(queued_concurrency.max(1))),
        }
    }

    /// Register a listener for events of type `E`.
    ///
    /// **Append-only contract:** every call pushes another listener — there is
    /// deliberately no dedup, so a caller can register two instances of the
    /// same listener type with different state. The flip side is that calling
    /// `listen` (or [`Event::broadcast`]) twice for the same listener delivers
    /// twice; register listeners exactly once, from a bootstrap path that runs
    /// once (tests reset via `TestContainerGuard`).
    ///
    /// **Poison policy** (Domain 11 audit D11-A): if the listener
    /// registry's `RwLock` is poisoned, the registration is skipped
    /// and a `tracing::error!` is emitted. Production: the listener
    /// that couldn't register surfaces to ops via the log; framework
    /// stays alive.
    pub async fn listen<E, L>(&self, listener: Arc<L>)
    where
        E: super::Event,
        L: Listener<E>,
    {
        let wrap = Arc::new(ListenerWrap::<E, L>::new(listener)) as Arc<dyn ErasedListener>;
        match self.listeners.write() {
            Ok(mut map) => {
                map.entry(TypeId::of::<E>()).or_default().push(wrap);
            }
            Err(_) => {
                tracing::error!(
                    event_type = std::any::type_name::<E>(),
                    "EventDispatcher listener registry lock poisoned; \
                     skipping listener registration. Events of this type \
                     will dispatch with no listener invoked."
                );
            }
        }
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
    /// in registration order, fail-fast on the first listener error).
    /// Queued events spawn a bounded, retrying task per listener; this
    /// call returns after spawning, not after they complete — but it
    /// will await a backpressure permit first, so under a flood of
    /// queued events `dispatch` slows rather than spawning unbounded
    /// tasks.
    ///
    /// **Poison policy** (Domain 11 audit D11-A): if the listener
    /// registry lock is poisoned, dispatch returns `Ok(())` after
    /// logging an error — equivalent to "no listeners registered for
    /// this event type", which is the documented safe-fallback
    /// semantic (events are not guaranteed to have subscribers).
    pub async fn dispatch<E: super::Event>(&self, event: E) -> Result<(), FrameworkError> {
        let listeners = match self.listeners.read() {
            Ok(map) => map.get(&TypeId::of::<E>()).cloned().unwrap_or_default(),
            Err(_) => {
                tracing::error!(
                    event = E::event_name(),
                    "EventDispatcher listener registry lock poisoned during dispatch; \
                     treating as no listeners (event dropped silently apart from this log)."
                );
                return Ok(());
            }
        };

        debug!(
            event = E::event_name(),
            listeners = listeners.len(),
            queued = E::queued(),
            "dispatching event"
        );

        if E::queued() {
            for l in listeners {
                self.spawn_queued_listener::<E>(l, event.clone()).await;
            }
        } else {
            for l in listeners {
                l.dispatch(&event).await?;
            }
        }

        Ok(())
    }

    /// Dispatch an event to every listener **best-effort**: run all of them
    /// even if some return `Err`, collecting the first error to return after
    /// the rest have run. Contrast [`dispatch`](Self::dispatch), which is
    /// fail-fast for synchronous events (stops at the first error — the
    /// semantic cancellable model events depend on).
    ///
    /// Queued events behave identically under both methods (each listener is
    /// an independent task); the distinction only matters for synchronous
    /// events with multiple listeners.
    pub async fn dispatch_best_effort<E: super::Event>(
        &self,
        event: E,
    ) -> Result<(), FrameworkError> {
        let listeners = match self.listeners.read() {
            Ok(map) => map.get(&TypeId::of::<E>()).cloned().unwrap_or_default(),
            Err(_) => {
                tracing::error!(
                    event = E::event_name(),
                    "EventDispatcher listener registry lock poisoned during dispatch; \
                     treating as no listeners."
                );
                return Ok(());
            }
        };

        debug!(
            event = E::event_name(),
            listeners = listeners.len(),
            queued = E::queued(),
            "dispatching event (best-effort)"
        );

        if E::queued() {
            for l in listeners {
                self.spawn_queued_listener::<E>(l, event.clone()).await;
            }
            return Ok(());
        }

        let mut first_err: Option<FrameworkError> = None;
        for l in listeners {
            if let Err(e) = l.dispatch(&event).await {
                error!(event = E::event_name(), error = %e, "listener failed (best-effort; continuing)");
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Acquire a backpressure permit, then spawn the listener into the
    /// drainable task set with a bounded retry loop. The permit is acquired
    /// **before** the spawn so the semaphore actually bounds concurrency
    /// (acquiring inside the task would spawn unconditionally); it then moves
    /// into the task and releases on completion. The retry loop lives inside
    /// the single task so all attempts share one permit and one drain slot.
    async fn spawn_queued_listener<E: super::Event>(
        &self,
        listener: Arc<dyn ErasedListener>,
        event: E,
    ) {
        let permit = match Arc::clone(&self.queued_permits).acquire_owned().await {
            Ok(p) => p,
            // The semaphore is only closed if we explicitly close it, which we
            // never do; treat a closed semaphore as "do not spawn".
            Err(_) => return,
        };

        let mut tasks = self.queued_tasks.lock().await;
        tasks.spawn(async move {
            let _permit = permit; // released when the task ends
            let mut attempt: u32 = 1;
            loop {
                match listener.dispatch(&event).await {
                    Ok(()) => return,
                    Err(e) if attempt >= MAX_QUEUED_ATTEMPTS => {
                        error!(
                            event = E::event_name(),
                            attempts = attempt,
                            error = %e,
                            "queued listener failed after retries; giving up"
                        );
                        return;
                    }
                    Err(e) => {
                        let backoff = retry_backoff(attempt);
                        warn!(
                            event = E::event_name(),
                            attempt,
                            retry_in_ms = backoff.as_millis() as u64,
                            error = %e,
                            "queued listener failed; retrying"
                        );
                        tokio::time::sleep(backoff).await;
                        attempt += 1;
                    }
                }
            }
        });
    }

    /// Wait for in-flight queued-listener tasks to finish, up to `timeout`.
    /// Intended for the server's graceful-shutdown sequence so a deploy does
    /// not cut off best-effort listeners mid-flight. Returns the number of
    /// tasks still running when the deadline elapsed (`0` = fully drained);
    /// any stragglers past the deadline are aborted so shutdown cannot hang.
    ///
    /// The task set is taken out under the lock and drained without holding
    /// it, so a listener that itself dispatches a queued event cannot deadlock
    /// against the drain.
    pub async fn drain_queued(&self, timeout: Duration) -> usize {
        let mut set = {
            let mut guard = self.queued_tasks.lock().await;
            std::mem::replace(&mut *guard, JoinSet::new())
        };
        if set.is_empty() {
            return 0;
        }
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                next = set.join_next() => {
                    if next.is_none() {
                        return 0; // all drained
                    }
                }
                _ = &mut deadline => {
                    let remaining = set.len();
                    set.abort_all();
                    return remaining;
                }
            }
        }
    }
}

/// In-process exponential backoff for queued-listener retries: base 100ms,
/// doubling, capped at 2s, with full jitter (uniform in `[0, capped]`). Short
/// by design — these are in-memory transient-fault retries, not the durable
/// queue's minutes-long schedule.
fn retry_backoff(attempt: u32) -> Duration {
    let base_ms: u64 = 100;
    let raw = base_ms.saturating_mul(1u64 << (attempt.saturating_sub(1)).min(6));
    let capped = raw.min(2_000);
    let jittered = rand::rng().random_range(0..=capped);
    Duration::from_millis(jittered)
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

    /// Best-effort variant of [`dispatch`](Self::dispatch): run every
    /// synchronous listener even if some fail, returning the first error.
    pub async fn dispatch_best_effort<E: super::Event>(event: E) -> Result<(), FrameworkError> {
        if super::testing::is_active() {
            super::testing::record(event);
            return Ok(());
        }
        global().dispatch_best_effort(event).await
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

    /// Drain the global dispatcher's in-flight queued-listener tasks, up to
    /// `timeout`. Called from the server's graceful-shutdown sequence. Returns
    /// the count still running at the deadline (`0` = fully drained).
    pub async fn drain_queued(timeout: Duration) -> usize {
        global().drain_queued(timeout).await
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
    use crate::FrameworkError;
    use crate::events::{Event as EventTrait, Listener};
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

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
        d.listen::<Pinged, _>(Arc::new(Counter(count.clone())))
            .await;
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

    struct FailingCounter {
        fail: bool,
        count: Arc<AtomicI64>,
    }
    #[async_trait]
    impl Listener<Pinged> for FailingCounter {
        async fn handle(&self, event: &Pinged) -> Result<(), FrameworkError> {
            self.count.fetch_add(event.n, Ordering::SeqCst);
            if self.fail {
                Err(FrameworkError::internal("boom"))
            } else {
                Ok(())
            }
        }
    }

    #[tokio::test]
    async fn dispatch_is_fail_fast_and_stops_at_first_error() {
        let d = EventDispatcher::new();
        let a = Arc::new(AtomicI64::new(0));
        let b = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged, _>(Arc::new(FailingCounter {
            fail: true,
            count: a.clone(),
        }))
        .await;
        d.listen::<Pinged, _>(Arc::new(FailingCounter {
            fail: false,
            count: b.clone(),
        }))
        .await;
        let r = d.dispatch(Pinged { n: 1 }).await;
        assert!(r.is_err());
        assert_eq!(a.load(Ordering::SeqCst), 1);
        assert_eq!(
            b.load(Ordering::SeqCst),
            0,
            "fail-fast: the second listener must NOT run after the first errors"
        );
    }

    #[tokio::test]
    async fn dispatch_best_effort_runs_all_listeners_and_returns_first_error() {
        let d = EventDispatcher::new();
        let a = Arc::new(AtomicI64::new(0));
        let b = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged, _>(Arc::new(FailingCounter {
            fail: true,
            count: a.clone(),
        }))
        .await;
        d.listen::<Pinged, _>(Arc::new(FailingCounter {
            fail: false,
            count: b.clone(),
        }))
        .await;
        let r = d.dispatch_best_effort(Pinged { n: 1 }).await;
        assert!(r.is_err(), "best-effort still surfaces the first error");
        assert_eq!(a.load(Ordering::SeqCst), 1);
        assert_eq!(
            b.load(Ordering::SeqCst),
            1,
            "best-effort: the second listener runs even though the first failed"
        );
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
        d.listen::<QueuedPing, _>(Arc::new(SlowCounter(n.clone())))
            .await;
        d.dispatch(QueuedPing).await.unwrap();
        // Immediately after dispatch returns, the slow listener has
        // not had time to complete (it sleeps 20ms).
        assert_eq!(n.load(Ordering::SeqCst), 0);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn drain_queued_waits_for_in_flight_listeners() {
        let d = EventDispatcher::new();
        let n = Arc::new(AtomicI64::new(0));
        d.listen::<QueuedPing, _>(Arc::new(SlowCounter(n.clone())))
            .await;
        d.dispatch(QueuedPing).await.unwrap();
        // The 20ms listener has not finished yet.
        assert_eq!(n.load(Ordering::SeqCst), 0);
        // Draining with a generous deadline blocks until it completes.
        let remaining = d.drain_queued(std::time::Duration::from_secs(5)).await;
        assert_eq!(remaining, 0, "drain should report all tasks finished");
        assert_eq!(n.load(Ordering::SeqCst), 1, "listener must have run to completion");
    }

    struct FailNTimes {
        remaining_failures: AtomicI64,
        succeeded: Arc<AtomicI64>,
    }
    #[async_trait]
    impl Listener<QueuedPing> for FailNTimes {
        async fn handle(&self, _event: &QueuedPing) -> Result<(), FrameworkError> {
            if self.remaining_failures.fetch_sub(1, Ordering::SeqCst) > 0 {
                return Err(FrameworkError::internal("transient"));
            }
            self.succeeded.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn queued_listener_retries_transient_failures() {
        let d = EventDispatcher::new();
        let succeeded = Arc::new(AtomicI64::new(0));
        d.listen::<QueuedPing, _>(Arc::new(FailNTimes {
            remaining_failures: AtomicI64::new(2), // fail twice, succeed on the 3rd attempt
            succeeded: succeeded.clone(),
        }))
        .await;
        d.dispatch(QueuedPing).await.unwrap();
        // Backoff is sub-second; draining waits for the retry sequence.
        let remaining = d.drain_queued(std::time::Duration::from_secs(5)).await;
        assert_eq!(remaining, 0);
        assert_eq!(
            succeeded.load(Ordering::SeqCst),
            1,
            "listener should ultimately succeed after retrying transient failures"
        );
    }

    struct Blocker {
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl Listener<QueuedPing> for Blocker {
        async fn handle(&self, _event: &QueuedPing) -> Result<(), FrameworkError> {
            self.started.notify_one();
            self.release.notified().await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn queued_dispatch_applies_backpressure_when_permits_exhausted() {
        // Capacity 1: a second queued dispatch cannot proceed until the first
        // listener task releases its permit.
        let d = Arc::new(EventDispatcher::with_concurrency(1));
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        d.listen::<QueuedPing, _>(Arc::new(Blocker {
            started: started.clone(),
            release: release.clone(),
        }))
        .await;

        // First dispatch: the listener task starts and holds the only permit.
        d.dispatch(QueuedPing).await.unwrap();
        started.notified().await;

        // Second dispatch must block on permit acquisition. Race it in a task
        // and prove it does not complete while the permit is held.
        let d2 = d.clone();
        let second = tokio::spawn(async move { d2.dispatch(QueuedPing).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !second.is_finished(),
            "second queued dispatch must wait for a free permit (backpressure)"
        );

        // Release the first listener → its permit frees → the second proceeds.
        release.notify_one();
        // The second listener also blocks on `release`; notify again so it can finish.
        release.notify_one();
        second.await.unwrap().unwrap();
        let remaining = d.drain_queued(std::time::Duration::from_secs(5)).await;
        assert_eq!(remaining, 0);
    }
}
