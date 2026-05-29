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

// Per-task deferred-dispatch buffer. When set (via [`EventDispatcher::defer`]
// or the [`Event::defer`] facade), every `dispatch`/`dispatch_best_effort`
// call that targets an eligible event type appends a boxed re-dispatch
// closure to this buffer instead of running the listeners. The deferring
// caller flushes after the callback completes. This is task-local so two
// concurrent `defer` calls cannot stomp on each other's buffers.
tokio::task_local! {
    static DEFER_BUFFER: DeferBuffer;
}

/// One deferred dispatch: a boxed re-dispatch closure that, given a borrowed
/// dispatcher, re-runs the original dispatch operation. Closes over a clone of
/// the event payload and the dispatch mode (`dispatch` vs `dispatch_best_effort`)
/// from the original call site, captured while the concrete `E` was still in
/// scope. Returns the same `Result` shape the original call would have returned.
///
/// Used by both [`EventDispatcher::defer`] (task-local buffer) and
/// [`EventDispatcher::push`] (per-dispatcher named bucket flushed by
/// [`EventDispatcher::flush`]).
type DeferredCall = Box<
    dyn for<'a> FnOnce(
            &'a EventDispatcher,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<(), FrameworkError>> + Send + 'a>,
        > + Send
        + 'static,
>;

#[derive(Clone)]
struct DeferBuffer {
    inner: Arc<TokioMutex<DeferBufferInner>>,
}

struct DeferBufferInner {
    /// Names of event types that may be deferred; `None` defers ALL events.
    only: Option<std::collections::HashSet<&'static str>>,
    /// Recorded re-dispatch closures, in the order they were captured.
    pending: Vec<DeferredCall>,
}

impl DeferBuffer {
    fn new(only: Option<std::collections::HashSet<&'static str>>) -> Self {
        Self {
            inner: Arc::new(TokioMutex::new(DeferBufferInner {
                only,
                pending: Vec::new(),
            })),
        }
    }
}

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
    /// Pushed events: stored as type-erased re-dispatch futures keyed by event
    /// name. [`Self::push`] serializes the event with its concrete dispatch
    /// closure; [`Self::flush`] drains the bucket and awaits each.
    pushed: TokioMutex<HashMap<&'static str, Vec<DeferredCall>>>,
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
            pushed: TokioMutex::new(HashMap::new()),
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
        // Record the registration with the fake so `assert_listening` can see
        // it. No-op when no fake is installed.
        super::testing::record_listener::<E, L>();
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
        if self
            .try_defer::<E, _>(&event, |ev| {
                Box::new(move |d: &EventDispatcher| {
                    Box::pin(d.dispatch_inner::<E>(ev))
                        as std::pin::Pin<
                            Box<dyn Future<Output = Result<(), FrameworkError>> + Send + '_>,
                        >
                })
            })
            .await
        {
            return Ok(());
        }

        self.dispatch_inner(event).await
    }

    /// Listener iteration without the deferral / push hooks. The public
    /// `dispatch` first checks the task-local defer buffer; deferred entries
    /// re-call `dispatch_inner` at flush time so they cannot recurse back into
    /// the defer check. Same fail-fast semantics as `dispatch`.
    async fn dispatch_inner<E: super::Event>(&self, event: E) -> Result<(), FrameworkError> {
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

    /// If a deferral scope is active in this task and the event is eligible,
    /// clone the event into the buffer and return `true` (caller short-circuits
    /// with `Ok(())`). Returns `false` when no deferral is active or the event
    /// is filtered out — caller proceeds with normal dispatch.
    ///
    /// `build_call` is invoked only when we are deferring; it builds the
    /// re-dispatch closure that the buffer will flush. Keeping the closure
    /// build behind a callback avoids a Box allocation on the hot path.
    async fn try_defer<E, F>(&self, event: &E, build_call: F) -> bool
    where
        E: super::Event,
        F: FnOnce(E) -> DeferredCall,
    {
        let Ok(buffer) = DEFER_BUFFER.try_with(|b| b.clone()) else {
            return false;
        };
        let mut guard = buffer.inner.lock().await;
        if let Some(only) = &guard.only
            && !only.contains(E::event_name())
        {
            return false;
        }
        guard.pending.push(build_call(event.clone()));
        true
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
        if self
            .try_defer::<E, _>(&event, |ev| {
                Box::new(move |d: &EventDispatcher| {
                    Box::pin(d.dispatch_best_effort_inner::<E>(ev))
                        as std::pin::Pin<
                            Box<dyn Future<Output = Result<(), FrameworkError>> + Send + '_>,
                        >
                })
            })
            .await
        {
            return Ok(());
        }

        self.dispatch_best_effort_inner(event).await
    }

    async fn dispatch_best_effort_inner<E: super::Event>(
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

    /// True when at least one listener is registered for event type `E`.
    /// Mirrors Laravel's `Dispatcher::hasListeners($eventName)`. A poisoned
    /// registry lock is treated as "no listeners" — same fail-safe stance as
    /// [`dispatch`](Self::dispatch).
    pub fn has_listeners<E: super::Event>(&self) -> bool {
        match self.listeners.read() {
            Ok(map) => map
                .get(&TypeId::of::<E>())
                .map(|v| !v.is_empty())
                .unwrap_or(false),
            Err(_) => {
                tracing::error!(
                    event = E::event_name(),
                    "EventDispatcher listener registry lock poisoned during has_listeners; \
                     reporting false"
                );
                false
            }
        }
    }

    /// Remove every listener registered for event type `E`. Mirrors Laravel's
    /// `Dispatcher::forget($event)`. Returns the number of listeners removed.
    ///
    /// In production code this is rarely the right tool — listener registration
    /// is normally bootstrap-once. It exists for test isolation (alongside the
    /// process-wide [`clear`](Self::clear)) and for code that hot-swaps a
    /// listener at runtime (e.g. switching a notifier mid-process).
    pub fn forget<E: super::Event>(&self) -> usize {
        match self.listeners.write() {
            Ok(mut map) => map.remove(&TypeId::of::<E>()).map(|v| v.len()).unwrap_or(0),
            Err(_) => {
                tracing::error!(
                    event = E::event_name(),
                    "EventDispatcher listener registry lock poisoned during forget; no-op"
                );
                0
            }
        }
    }

    /// Record an event to be dispatched later via [`flush`](Self::flush).
    /// Mirrors Laravel's `Dispatcher::push($event, $payload)` / `flush($event)`
    /// pair: useful when you want to capture an event during a request but
    /// defer firing it until a specific point (e.g. after rendering, before
    /// background-work scheduling).
    ///
    /// Each call appends to the per-event bucket; [`flush`](Self::flush) drains
    /// that bucket and dispatches the events in the order they were pushed.
    /// Pushed events do NOT participate in the [`defer`](Self::defer) scope —
    /// they are already explicitly deferred.
    pub async fn push<E: super::Event>(&self, event: E) {
        let mut guard = self.pushed.lock().await;
        guard
            .entry(E::event_name())
            .or_default()
            .push(Box::new(move |d: &EventDispatcher| {
                Box::pin(d.dispatch_inner::<E>(event))
                    as std::pin::Pin<
                        Box<dyn Future<Output = Result<(), FrameworkError>> + Send + '_>,
                    >
            }));
    }

    /// Drain and dispatch every event previously [`push`](Self::push)ed under
    /// the name `E::event_name()`. Returns the first dispatch error if any of
    /// the pushed events fail; the rest still run (drain semantics — not
    /// fail-fast).
    ///
    /// Calling `flush` for an event name with no pushed entries is a no-op.
    pub async fn flush<E: super::Event>(&self) -> Result<(), FrameworkError> {
        let pending = {
            let mut guard = self.pushed.lock().await;
            guard.remove(E::event_name()).unwrap_or_default()
        };
        let mut first_err: Option<FrameworkError> = None;
        for call in pending {
            if let Err(e) = call(self).await
                && first_err.is_none()
            {
                first_err = Some(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Forget every pushed event without dispatching. Mirrors Laravel's
    /// `Dispatcher::forgetPushed()`. Returns the number of pushed events
    /// dropped (summed across all event names).
    pub async fn forget_pushed(&self) -> usize {
        let mut guard = self.pushed.lock().await;
        let total: usize = guard.values().map(|v| v.len()).sum();
        guard.clear();
        total
    }

    /// Execute `callback` while buffering every dispatch into a task-local
    /// queue; after the callback returns, drain the queue and dispatch each
    /// buffered event in the order it was captured. Mirrors Laravel's
    /// `Dispatcher::defer($callback, $events)`.
    ///
    /// Pass `only = None` to defer ALL events; pass `Some(&["UserRegistered",
    /// ...])` to defer only events whose name matches one of the entries.
    /// Events not in the filter dispatch normally inside `callback`.
    ///
    /// The first dispatch error during flush is returned; remaining buffered
    /// events still run. Errors raised inside `callback` propagate untouched.
    pub async fn defer<F, T>(
        &self,
        only: Option<&[&'static str]>,
        callback: F,
    ) -> Result<(T, Option<FrameworkError>), FrameworkError>
    where
        F: std::future::Future<Output = Result<T, FrameworkError>> + Send,
    {
        let only_set = only.map(|names| {
            names
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
        });
        let buffer = DeferBuffer::new(only_set);
        let buffer_clone = buffer.clone();
        let value = DEFER_BUFFER.scope(buffer_clone, callback).await?;
        // The callback completed Ok; drain the buffer and dispatch.
        let pending = std::mem::take(&mut buffer.inner.lock().await.pending);
        let mut first_err: Option<FrameworkError> = None;
        for call in pending {
            if let Err(e) = call(self).await
                && first_err.is_none()
            {
                first_err = Some(e);
            }
        }
        Ok((value, first_err))
    }

    /// Register a [`Subscriber`] — a single struct that bundles a related set
    /// of listener registrations behind one bootstrap call. Mirrors Laravel's
    /// `Dispatcher::subscribe($subscriber)`.
    ///
    /// Each subscriber owns its own state (database handle, config, ...) and
    /// chooses which events to attach in [`Subscriber::subscribe`]. The
    /// dispatcher passes itself to the subscribe method so the subscriber can
    /// call back into [`listen`](Self::listen).
    pub async fn subscribe<S: super::Subscriber>(&self, subscriber: Arc<S>) {
        subscriber.subscribe(self).await;
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
        if super::testing::is_active::<E>() {
            super::testing::record(event);
            return Ok(());
        }
        global().dispatch(event).await
    }

    /// Best-effort variant of [`dispatch`](Self::dispatch): run every
    /// synchronous listener even if some fail, returning the first error.
    pub async fn dispatch_best_effort<E: super::Event>(event: E) -> Result<(), FrameworkError> {
        if super::testing::is_active::<E>() {
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

    /// True when at least one listener is registered on the global dispatcher
    /// for event type `E`. Mirrors Laravel's `Event::hasListeners($name)`.
    pub fn has_listeners<E: super::Event>() -> bool {
        global().has_listeners::<E>()
    }

    /// Remove every listener for event type `E` from the global dispatcher.
    /// Returns the number of listeners removed. Mirrors Laravel's
    /// `Event::forget($event)`.
    pub fn forget<E: super::Event>() -> usize {
        global().forget::<E>()
    }

    /// Record an event for later dispatch on the global dispatcher.
    /// Mirrors Laravel's `Event::push($event, $payload)`. Pair with
    /// [`flush`](Self::flush) to drain.
    ///
    /// When a fake is active, `push` is a no-op — Laravel's `EventFake::push`
    /// is also a deliberate no-op.
    pub async fn push<E: super::Event>(event: E) {
        if super::testing::fake_installed() {
            return;
        }
        global().push(event).await;
    }

    /// Drain and dispatch every pushed event of type `E` on the global
    /// dispatcher. Mirrors Laravel's `Event::flush($event)`. Returns the first
    /// dispatch error if any pushed event fails; remaining pushed events still
    /// run.
    pub async fn flush<E: super::Event>() -> Result<(), FrameworkError> {
        if super::testing::fake_installed() {
            return Ok(());
        }
        global().flush::<E>().await
    }

    /// Forget every pushed event without dispatching. Mirrors Laravel's
    /// `Event::forgetPushed()`. Returns the number of pushed events dropped.
    pub async fn forget_pushed() -> usize {
        global().forget_pushed().await
    }

    /// Run `callback` while buffering every dispatch into a task-local queue,
    /// then drain the queue and dispatch each buffered event in capture order.
    /// Mirrors Laravel's `Event::defer($callback, $events)`.
    ///
    /// Returns the callback's value and the first flush error (if any).
    pub async fn defer<F, T>(
        only: Option<&[&'static str]>,
        callback: F,
    ) -> Result<(T, Option<FrameworkError>), FrameworkError>
    where
        F: std::future::Future<Output = Result<T, FrameworkError>> + Send,
    {
        global().defer(only, callback).await
    }

    /// Register a [`Subscriber`](super::Subscriber) on the global dispatcher.
    /// Mirrors Laravel's `Event::subscribe($subscriber)`. The subscriber's
    /// `subscribe` method runs once and attaches every listener it owns.
    pub async fn subscribe<S: super::Subscriber>(subscriber: Arc<S>) {
        global().subscribe(subscriber).await;
    }

    /// Run `callback` with a "null" dispatcher in scope: every dispatch made
    /// inside the callback is silently discarded. Mirrors Laravel's
    /// `Event::fake()` minus the recording, similar in spirit to
    /// `NullDispatcher`. Useful in tests where you want to suppress events
    /// without paying for the fake's record-and-assert machinery.
    pub async fn muted<F, T>(callback: F) -> T
    where
        F: std::future::Future<Output = T> + Send,
    {
        super::testing::muted(callback).await
    }

    /// Replace the global dispatcher with a fake. Returns a guard
    /// that restores listener invocation on drop. Available to
    /// consumer-crate tests by default — no feature gate.
    pub fn fake() -> super::testing::EventFakeGuard {
        super::testing::install_fake()
    }

    /// Replace the global dispatcher with a fake that intercepts only the
    /// named event types; everything else passes through to the real
    /// dispatcher. Mirrors Laravel's `Event::fakeOnly($events)` (which is the
    /// `EventFake::eventsToFake` constructor argument).
    pub fn fake_only(events_to_fake: &[&'static str]) -> super::testing::EventFakeGuard {
        super::testing::install_fake_only(events_to_fake)
    }

    /// Replace the global dispatcher with a fake that intercepts every event
    /// EXCEPT the named ones; the excepted events pass through to the real
    /// dispatcher. Mirrors Laravel's `EventFake::except($events)`.
    pub fn fake_except(events_to_dispatch: &[&'static str]) -> super::testing::EventFakeGuard {
        super::testing::install_fake_except(events_to_dispatch)
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
        assert_eq!(
            n.load(Ordering::SeqCst),
            1,
            "listener must have run to completion"
        );
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

    // ---------- Laravel-13 parity: has_listeners / forget --------------------

    #[tokio::test]
    async fn has_listeners_is_false_before_listen_and_true_after() {
        let d = EventDispatcher::new();
        assert!(!d.has_listeners::<Pinged>());
        d.listen::<Pinged, _>(Arc::new(Counter(Arc::new(AtomicI64::new(0)))))
            .await;
        assert!(d.has_listeners::<Pinged>());
    }

    #[tokio::test]
    async fn forget_removes_all_listeners_and_returns_count() {
        let d = EventDispatcher::new();
        d.listen::<Pinged, _>(Arc::new(Counter(Arc::new(AtomicI64::new(0)))))
            .await;
        d.listen::<Pinged, _>(Arc::new(Counter(Arc::new(AtomicI64::new(0)))))
            .await;
        assert!(d.has_listeners::<Pinged>());
        let removed = d.forget::<Pinged>();
        assert_eq!(removed, 2);
        assert!(!d.has_listeners::<Pinged>());
    }

    #[tokio::test]
    async fn forget_on_unused_event_returns_zero() {
        let d = EventDispatcher::new();
        assert_eq!(d.forget::<Pinged>(), 0);
    }

    // ---------- Laravel-13 parity: push / flush / forget_pushed -------------

    #[tokio::test]
    async fn push_buffers_and_flush_dispatches() {
        let d = EventDispatcher::new();
        let count = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged, _>(Arc::new(Counter(count.clone())))
            .await;
        d.push(Pinged { n: 1 }).await;
        d.push(Pinged { n: 2 }).await;
        d.push(Pinged { n: 3 }).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "push must not call listeners until flush"
        );
        d.flush::<Pinged>().await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 6);
    }

    #[tokio::test]
    async fn flush_drains_bucket_so_a_second_flush_is_a_noop() {
        let d = EventDispatcher::new();
        let count = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged, _>(Arc::new(Counter(count.clone())))
            .await;
        d.push(Pinged { n: 7 }).await;
        d.flush::<Pinged>().await.unwrap();
        d.flush::<Pinged>().await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 7);
    }

    #[tokio::test]
    async fn forget_pushed_drops_buffered_events_without_dispatching() {
        let d = EventDispatcher::new();
        let count = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged, _>(Arc::new(Counter(count.clone())))
            .await;
        d.push(Pinged { n: 1 }).await;
        d.push(Pinged { n: 2 }).await;
        let dropped = d.forget_pushed().await;
        assert_eq!(dropped, 2);
        d.flush::<Pinged>().await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    // ---------- Laravel-13 parity: defer ------------------------------------

    #[tokio::test]
    async fn defer_buffers_dispatches_and_flushes_after_callback() {
        // The deferred closure re-invokes through the dispatcher reference
        // passed to flush, so a callback on the local dispatcher running
        // `d.dispatch(...)` directly buffers and flushes to the same
        // dispatcher.
        let d = EventDispatcher::new();
        let count = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged, _>(Arc::new(Counter(count.clone())))
            .await;
        let mid = Arc::new(AtomicI64::new(-1));
        let mid_w = mid.clone();
        let d_ref = &d;
        let count_clone = count.clone();
        let ((), flush_err) = d
            .defer::<_, ()>(None, async move {
                d_ref.dispatch(Pinged { n: 10 }).await?;
                d_ref.dispatch(Pinged { n: 20 }).await?;
                mid_w.store(count_clone.load(Ordering::SeqCst), Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap();
        assert!(flush_err.is_none());
        assert_eq!(
            mid.load(Ordering::SeqCst),
            0,
            "events must not fire before the callback returns"
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            30,
            "events must fire after the callback returns"
        );
    }

    #[tokio::test]
    async fn defer_only_passes_through_unlisted_events() {
        let d = EventDispatcher::new();
        let count = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged, _>(Arc::new(Counter(count.clone())))
            .await;
        let mid = Arc::new(AtomicI64::new(-1));
        let mid_w = mid.clone();
        let d_ref = &d;
        let count_clone = count.clone();
        // Defer only "OtherEvent" — our Pinged event passes through immediately.
        let ((), flush_err) = d
            .defer::<_, ()>(Some(&["OtherEvent"]), async move {
                d_ref.dispatch(Pinged { n: 5 }).await?;
                mid_w.store(count_clone.load(Ordering::SeqCst), Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap();
        assert!(flush_err.is_none());
        assert_eq!(
            mid.load(Ordering::SeqCst),
            5,
            "Pinged should fire inline because it is NOT in the only-list"
        );
    }

    #[tokio::test]
    async fn defer_propagates_callback_error_without_running_buffered_events() {
        let d = EventDispatcher::new();
        let count = Arc::new(AtomicI64::new(0));
        d.listen::<Pinged, _>(Arc::new(Counter(count.clone())))
            .await;
        let d_ref = &d;
        let result = d
            .defer::<_, ()>(None, async move {
                d_ref.dispatch(Pinged { n: 1 }).await?;
                Err(FrameworkError::internal("callback bombed"))
            })
            .await;
        assert!(result.is_err());
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "buffered events must not flush when the callback errors"
        );
    }

    // ---------- Laravel-13 parity: subscribe --------------------------------

    struct TestSubscriber {
        marker: Arc<AtomicI64>,
    }
    #[async_trait]
    impl super::super::Subscriber for TestSubscriber {
        async fn subscribe(self: Arc<Self>, dispatcher: &EventDispatcher) {
            let marker = self.marker.clone();
            dispatcher
                .listen::<Pinged, _>(Arc::new(Counter(marker)))
                .await;
        }
    }

    #[tokio::test]
    async fn subscriber_attaches_every_listener_it_owns() {
        let d = EventDispatcher::new();
        let count = Arc::new(AtomicI64::new(0));
        d.subscribe(Arc::new(TestSubscriber {
            marker: count.clone(),
        }))
        .await;
        d.dispatch(Pinged { n: 9 }).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 9);
    }
}
