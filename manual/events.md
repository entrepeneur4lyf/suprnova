# Events

Events are Suprnova's typed in-process pub/sub. A controller fires
`UserRegistered { user_id }`; one listener emails the user, another
writes an audit row, a third publishes a broadcast. All three see the
same payload, run in registration order, and have no compile-time
knowledge of each other.

The user-facing surface is the `EventFacade` struct (re-exported as
`suprnova::EventFacade`). The crate also re-exports the `Event` *trait*
as `suprnova::Event` — same name as Laravel's facade, but in Rust the
trait is the typed contract every payload implements. Behind the facade
is a single process-global `EventDispatcher` (held in a `OnceLock`):
registered listeners survive the request that registered them, and
dispatches either run inline or spawn into a bounded retrying task set.

## The basics

```rust
use suprnova::{EventFacade, Event, Listener, FrameworkError, async_trait};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct UserRegistered {
    pub user_id: i64,
}

impl Event for UserRegistered {
    fn event_name() -> &'static str {
        "UserRegistered"
    }
}

pub struct SendWelcomeEmail;

#[async_trait]
impl Listener<UserRegistered> for SendWelcomeEmail {
    async fn handle(&self, e: &UserRegistered) -> Result<(), FrameworkError> {
        // send the email…
        let _ = e.user_id;
        Ok(())
    }
}

// In bootstrap.rs:
EventFacade::listen::<UserRegistered, SendWelcomeEmail>(Arc::new(SendWelcomeEmail)).await;

// In a controller:
EventFacade::dispatch(UserRegistered { user_id: 42 }).await?;
```

`Event` requires `Send + Sync + Clone + 'static + Debug` so a payload
can cross task boundaries (queued listeners) and the dispatcher can
log it. `Listener<E>` is `Send + Sync + 'static` so it can outlive the
registration call. There is no `#[derive(Event)]` — the trait has two
methods (`event_name` and the defaulted `queued`) so a hand-written
impl is two lines.

## Dispatch modes

| Method | Semantics |
|---|---|
| `EventFacade::dispatch(event)` | Synchronous, fail-fast — the first listener `Err` aborts the chain |
| `EventFacade::dispatch_best_effort(event)` | Synchronous, run-them-all — returns the first `Err` after every listener has run |
| `EventFacade::dispatch(event)` when `Event::queued() = true` | Each listener spawns as a bounded retrying task; the call returns after spawning |

Use `dispatch` (fail-fast) when a downstream side effect MUST observe
a successful upstream — most model lifecycle hooks fall here, so an
observer that vetoes a save can short-circuit. Use
`dispatch_best_effort` for fan-out where one failing listener should
not silence the rest — most observability events fall here.

Override the trait method to opt into queued delivery:

```rust
impl Event for ExpensiveAuditTrail {
    fn event_name() -> &'static str { "ExpensiveAuditTrail" }
    fn queued() -> bool { true }
}
```

Queued listeners are bounded by a process-wide semaphore. The default
ceiling is 256 concurrent tasks; override per dispatcher with
`EventDispatcher::with_concurrency(n)` or globally via the
`EVENT_MAX_CONCURRENCY` env var. Each task retries up to 3 attempts
with 100ms→2s jittered backoff before giving up — these are in-process
transient-fault retries, not the durable queue's minutes-long schedule.

## Subscribers — bundle related registrations

When several listeners belong to one feature, a `Subscriber`
registers them as a unit. Mirrors Laravel's `EventServiceProvider`
subscriber pattern.

```rust
use suprnova::{EventFacade, EventDispatcher, Subscriber, async_trait};
use std::sync::Arc;

pub struct UserEventSubscriber {
    db: Arc<crate::Db>,
}

#[async_trait]
impl Subscriber for UserEventSubscriber {
    async fn subscribe(self: Arc<Self>, d: &EventDispatcher) {
        let db = self.db.clone();
        d.listen::<UserRegistered, _>(Arc::new(SendWelcomeEmail::new(db.clone()))).await;
        d.listen::<UserDeleted, _>(Arc::new(CleanupUserData::new(db.clone()))).await;
        d.listen::<UserPromoted, _>(Arc::new(NotifyAdmins::new(db))).await;
    }
}

// In bootstrap.rs — one line per subscriber instead of three per listener:
EventFacade::subscribe(Arc::new(UserEventSubscriber { db: db.clone() })).await;
```

`subscribe` takes `Arc<S>` so listeners that need to share state with
the subscriber can clone the `Arc` and capture it.

## Inspecting and removing listeners

```rust
if EventFacade::has_listeners::<UserRegistered>() {
    EventFacade::dispatch(UserRegistered { user_id: 42 }).await?;
}

let removed: usize = EventFacade::forget::<UserRegistered>();
```

`has_listeners::<E>()` mirrors Laravel's
`Event::hasListeners($eventName)`. `forget::<E>()` drops every
listener registered for that event type and returns the count
removed. Production code rarely needs `forget` — listener registration
is normally bootstrap-once — but hot-swap and test code reach for it.

Both methods return safe defaults when the listener registry lock is
poisoned (`false` and `0` respectively), with a `tracing::error!`
logged so the failure is observable.

## Push and flush

`push` captures an event in a per-event-name bucket without firing
it. `flush::<E>()` drains the bucket and dispatches everything in
capture order. Mirrors Laravel's `Event::push` / `Event::flush` pair.

```rust
// Inside a handler that does work in two phases:
EventFacade::push(UserRegistered { user_id: 42 }).await;
// … rendering, validation, more work …
EventFacade::flush::<UserRegistered>().await?;
```

Pushed events ignore the `defer` scope — they are already explicitly
deferred. `forget_pushed()` drops every pushed event without
dispatching, returning the count dropped. Mirrors
`Event::forgetPushed()`.

## defer — buffer every dispatch inside a callback

`defer(only, async { … })` runs the callback with a task-local
buffer in scope. Every `dispatch` / `dispatch_best_effort` call made
inside the callback is captured and replayed after the callback
returns. Mirrors Laravel's `Event::defer($callback, ?$events)`.

```rust
let ((), flush_err) = EventFacade::defer::<_, ()>(None, async {
    do_work_part_one().await?;
    EventFacade::dispatch(WorkStarted).await?; // buffered
    do_work_part_two().await?;
    EventFacade::dispatch(WorkFinished).await?; // buffered
    Ok(())
})
.await?;
// At this point WorkStarted and WorkFinished have both fired in order.
// `flush_err` carries the first dispatch error from the replay (if any).
```

Pass `Some(&["EventOne", "EventTwo"])` to defer ONLY those event
names; everything else dispatches inline as usual. A callback error
short-circuits — buffered events are dropped, the error propagates.

The defer buffer is per-Tokio-task, so two concurrent `defer` calls
don't stomp on each other's state.

## Queued listeners — in-process vs durable

Two distinct "queued" tiers, and the naming matters:

| Need | Reach for |
|---|---|
| Listener should run off-task; OK to lose on crash | `Event::queued() = true` on the event trait |
| Listener work MUST survive a crash + restart | `QueuedListener<E, J>` (bridges event → durable job) |

`Event::queued() = true` makes the dispatcher spawn each listener as
its own Tokio task, bounded by a process semaphore, with bounded
retry (3 attempts, jittered backoff). The work runs on this process;
a crash drops in-flight listeners. The
[graceful-shutdown drain](#draining-on-shutdown) waits for in-flight
tasks up to a deadline.

`QueuedListener<E, J>` is a stock listener that builds a
[`Job`](queues.md) from each event and pushes it on the durable
queue. The event still fires synchronously; the listener just
enqueues — which is fast — so request latency stays low. The job
itself survives the crash because the queue is durable.

```rust
use suprnova::{EventFacade, QueuedListener};
use std::sync::Arc;

EventFacade::listen::<UserRegistered, _>(Arc::new(
    QueuedListener::<UserRegistered, SendWelcomeEmailJob>::new(|e| SendWelcomeEmailJob {
        user_id: e.user_id,
    }),
))
.await;
```

The `QueuedListener` only needs the event to be a regular synchronous
event — the durability lives in the queue, not the dispatcher.

## Draining on shutdown

Queued in-process listeners spawn into a `JoinSet` tracked by the
dispatcher. The server's graceful-shutdown sequence calls
`EventFacade::drain_queued(timeout)` to wait for them:

```rust
let still_running = EventFacade::drain_queued(Duration::from_secs(30)).await;
if still_running > 0 {
    tracing::warn!(still_running, "queued listeners abandoned at shutdown");
}
```

Drain returns the count still running when the deadline elapsed (`0`
= fully drained). Stragglers past the deadline are aborted so
shutdown cannot hang.

## Bridging events to broadcasting

`EventFacade::broadcast::<E>(hub)` wires a one-line bridge from a
dispatched event to a `BroadcastHub`. Any type that implements
`Broadcastable` and `Event` can be broadcast this way; listeners
receive the typed payload, and subscribers on the named channels
receive the broadcast envelope.

```rust
use suprnova::EventFacade;
use std::sync::Arc;

let hub: Arc<dyn suprnova::BroadcastHub> = Arc::new(broadcast_hub);
EventFacade::broadcast::<OrderShipped>(hub).await;

// Any later dispatch is also published to the channels declared
// by OrderShipped::broadcast_on():
EventFacade::dispatch(OrderShipped { order_id: 42, user_id: 99 }).await?;
```

See [Broadcasting](broadcasting.md) for the channel model
(public / private / presence) and the `Broadcastable` trait.

## Built-in events

The framework dispatches a fixed set of events from its own subsystems.
You opt in by registering listeners; if no listener is registered the
events are no-ops.

| Subsystem | Events | Dispatched by |
|---|---|---|
| Error handling | `ErrorOccurred` | Every 5xx response (returned `FrameworkError` or recovered panic) |
| Auth (guards) | `Auth\\Attempting`, `Auth\\Authenticated`, `Auth\\Login`, `Auth\\Logout`, `Auth\\Failed` | `StatefulGuard::attempt` / `login` / `logout` / `once` |
| Auth flows | `EmailVerified`, `PasswordResetLinkSent`, `PasswordResetCompleted`, `AccountLocked`, `AccountUnlocked`, `TwoFactorEnrolled`, `TwoFactorChallenged`, `TwoFactorChallengeFailed`, `TwoFactorDisabled` | `auth_flows::{EmailVerification, PasswordReset, BruteForce, TwoFactor}` |
| Database | `Database\\ConnectionEstablished`, `Database\\QueryExecuted`, `Database\\TransactionBeginning`, `Database\\TransactionCommitted`, `Database\\TransactionRolledBack`, `Database\\DatabaseBusy` | `DbConnection::connect`, `ExecutorChoice` helpers, `DB::transaction` |
| Mail | `Suprnova\\Mail\\MessageSending`, `Suprnova\\Mail\\MessageSent` | `MailBuilder::send` before/after transport |
| Notifications | `Suprnova::Notifications::Sending`, `Suprnova::Notifications::Sent`, `Suprnova::Notifications::Failed` | Each channel delivery |
| Queue (worker) | `queue::JobQueueing`, `JobQueued`, `JobProcessing`, `JobProcessed`, `JobAttempted`, `JobExceptionOccurred`, `JobFailed`, `JobReleased`, `JobReleasedAfterException`, `JobTimedOut`, `Looping`, `WorkerStarting`, `WorkerStopping`, `WorkerInterrupted` | `Queue::push` / `run_worker` |
| Features | `FeatureUpdated`, `FeatureDeleted` | `features::admin` CRUD |
| Eloquent (per model) | 16 lifecycle events — `Retrieved`, `Saving`, `Saved`, `Creating`, `Created`, `Updating`, `Updated`, `Deleting`, `Deleted`, `Restoring`, `Restored`, `ForceDeleting`, `ForceDeleted`, `Replicating`, `Pruning`, `Pruned` — emitted under each model's `events::` submodule | The `#[suprnova::model]` macro wires these into save/update/delete |

`ErrorOccurred` is the dedicated hook for shipping 5xx exceptions to
Sentry, Datadog, Slack, etc. The dispatch is best-effort and spawned,
so a broken Sentry listener cannot silence the rest, and response
conversion never blocks on it. See [Error Model](error-model.md) for
the full panic-recovery and conversion contract.

Model lifecycle events fire fail-fast: a `Saving` listener that
returns `EventResult::Cancel` (via the `CancellableListener` trait)
aborts the save. See [Eloquent observers and lifecycle events](eloquent.md).

## DB::listen — observing queries

For per-query observability you can register either a typed
`Listener<QueryExecuted>` through the dispatcher or, more commonly,
a `DB::listen` callback that mirrors Laravel's `DB::listen(function
($q) { ... })` signature:

```rust
use suprnova::DB;
use std::sync::Arc;

DB::listen(Arc::new(|q| {
    tracing::debug!(
        sql = %q.sql,
        time_ms = q.time.as_millis(),
        connection = %q.connection_name,
        "query"
    );
}));
```

The callback receives a `QueryExecuted` carrying the SQL, bindings,
wall-clock duration, connection name, the read/write classification,
and the final `Result` (so failed queries are observable too).
`QueryExecuted::to_raw_sql()` inlines bindings for log convenience —
debug-format, NOT SQL-safe.

Two re-entrancy and cost guarantees:

- **Re-entrancy guard.** A listener that itself issues a query won't
  re-fire `QueryExecuted` from that nested query — the dispatcher
  sets a task-local flag while a listener runs, and the executor
  skips emission inside that scope. A log-to-DB listener will not
  loop.
- **Zero overhead when nobody is listening.** The executor checks a
  combined `query_observation_active()` (any direct listener, any
  registered `Listener<QueryExecuted>`, OR query-log enabled) before
  building the event payload. When all three are off, the entire
  emission path is short-circuited.

## Testing — `EventFacade::fake()`

`EventFacade::fake()` swaps the global dispatcher with a recorder.
Dispatched events go into the recording instead of running listeners.
The fake holds a process-wide serializer for the lifetime of the
guard, so parallel `#[tokio::test]`s that use it run one at a time —
tests no longer need their own `serial_test` mutex.

```rust
use suprnova::events::{
    EventFacade, assert_dispatched, assert_dispatched_once, assert_dispatched_times,
    assert_nothing_dispatched, has_dispatched, dispatched, dispatched_events,
};

#[tokio::test]
async fn registration_dispatches_welcome_event() {
    let _guard = EventFacade::fake();

    register_user("ada@example.com").await.unwrap();

    assert_dispatched_once::<UserRegistered>();
    assert_dispatched::<UserRegistered>(|e| e.email == "ada@example.com");
}
```

| Helper | Asserts |
|---|---|
| `assert_dispatched::<E>(pred)` | at least one matching `E` was dispatched |
| `assert_dispatched_once::<E>()` | exactly one `E` was dispatched |
| `assert_dispatched_times::<E>(n)` | exactly `n` of `E` were dispatched |
| `assert_not_dispatched::<E>(pred)` | no matching `E` was dispatched |
| `assert_nothing_dispatched()` | NO events of any type were dispatched |
| `assert_listening::<E, L>()` | a listener `L` was registered for `E` |
| `has_dispatched::<E>()` | bool: any `E` recorded |
| `dispatched::<E>(pred)` | `Vec<E>` clones of matching events |
| `dispatched_count::<E>(pred)` | count of matching events |
| `dispatched_events()` | `HashMap<&'static str, usize>` of all dispatches |

### Selective faking

```rust
// Only fake these events; everything else dispatches normally.
let _guard = EventFacade::fake_only(&["UserRegistered", "UserDeleted"]);

// Fake every event EXCEPT these.
let _guard = EventFacade::fake_except(&["TelemetryEvent"]);
```

Mirrors Laravel's `Event::fake([…])` and `EventFake::except($events)`.

### Mute — discard events without recording

`EventFacade::muted(async { … })` runs the callback with a task-local
"silent dispatcher" flag set; every event dispatched inside is
discarded without recording or invoking listeners. The Suprnova
analogue of Laravel's `NullDispatcher`, scoped to a callback.

```rust
EventFacade::muted(async {
    // No listeners fire, no events recorded.
    run_bulk_import().await;
})
.await;
```

Unlike `fake()`, `muted` does NOT acquire the process serializer —
two muted scopes can run in parallel.

### `assert_listening` — verify a listener is wired up

Use to test bootstrap wiring without firing an event:

```rust
#[tokio::test]
async fn bootstrap_wires_welcome_listener() {
    let _guard = EventFacade::fake();
    bootstrap::register_listeners().await;
    suprnova::events::assert_listening::<UserRegistered, SendWelcomeEmail>();
}
```

The fake observes registrations via the dispatcher's `listen`
method, so the registration must happen INSIDE the fake's scope —
listeners registered before `EventFacade::fake()` are NOT seen by
`assert_listening`.

## Laravel parity reference

Every Laravel 13 `Event` facade and `EventFake` method that has a
typed-Rust equivalent ships under the closest matching name. Methods
Laravel exposes that don't fit typed Rust are omitted with a short
note.

| Laravel | Suprnova |
|---|---|
| `Event::dispatch($event)` | `EventFacade::dispatch(event).await` |
| `Event::dispatch($event)` (halt arg) | use `dispatch` (fail-fast on `Err`) |
| `Event::until($event)` | `dispatch` (typed: first `Err` halts) |
| `Event::listen($event, $listener)` | `EventFacade::listen::<E, L>(Arc::new(L))` |
| `Event::hasListeners($name)` | `EventFacade::has_listeners::<E>()` |
| `Event::forget($event)` | `EventFacade::forget::<E>()` |
| `Event::push($event)` | `EventFacade::push(event).await` |
| `Event::flush($event)` | `EventFacade::flush::<E>().await` |
| `Event::forgetPushed()` | `EventFacade::forget_pushed().await` |
| `Event::defer($callback, ?$events)` | `EventFacade::defer(only, async {…}).await` |
| `Event::subscribe($subscriber)` | `EventFacade::subscribe(Arc::new(S)).await` |
| `Event::fake()` | `EventFacade::fake()` (guard) |
| `Event::fake([$names])` | `EventFacade::fake_only(&["…"])` |
| `EventFake::except($names)` | `EventFacade::fake_except(&["…"])` |
| `EventFake::assertDispatched` | `assert_dispatched` |
| `EventFake::assertDispatchedOnce` | `assert_dispatched_once` |
| `EventFake::assertDispatchedTimes` | `assert_dispatched_times` |
| `EventFake::assertNotDispatched` | `assert_not_dispatched` |
| `EventFake::assertNothingDispatched` | `assert_nothing_dispatched` |
| `EventFake::assertListening` | `assert_listening` |
| `EventFake::hasDispatched` | `has_dispatched` |
| `EventFake::dispatched` | `dispatched` (returns `Vec<E>`) |
| `EventFake::dispatchedEvents` | `dispatched_events` (name → count map) |
| `NullDispatcher` | `EventFacade::muted(async {…}).await` |
| `Event::wildcards` (`User.*` patterns) | not shipped — use typed listeners, or the `Observer<M>` trait for per-model lifecycle hooks |
| `Event::subscribe` (string subscriber) | use the typed `Subscriber` trait |
| `DB::listen(function ($q) {…})` | `DB::listen(Arc::new(|q| {…}))` — same shape, takes `&QueryExecuted` |

### Why Suprnova diverges

Laravel's dispatcher leans on PHP's stringly-typed runtime: events
are class names passed as strings, listeners are class names looked
up via the container, and `Event::listen('User.*', ...)` works
because wildcards over class-name strings make sense in PHP. In
Rust, the equivalent of "this listener handles `User.*`" is "this
listener generic on `E: UserEvent`" — a trait, not a string match.
So Suprnova drops wildcards in favour of the type system, and the
result is that broken refactors become compile errors instead of
runtime mis-routes.

The other divergence is `defer`: Laravel's defer relies on the
request-per-process model to bound the deferral scope. Suprnova
serves many concurrent requests in one process, so the deferral
buffer is task-local. Two concurrent `defer` calls each get their
own buffer; the calls cannot stomp on each other, and there is no
hidden global state to leak.

## Where each piece lives

| Piece | File |
|---|---|
| `Event` trait, `Listener<E>`, `Subscriber` | `framework/src/events/mod.rs` |
| `EventDispatcher`, `EventFacade` (facade struct) | `framework/src/events/dispatcher.rs` |
| `ErrorOccurred` | `framework/src/events/builtins.rs` |
| `QueuedListener<E, J>` | `framework/src/events/queued_listener.rs` |
| `assert_dispatched*`, `EventFakeGuard`, `muted` | `framework/src/events/testing.rs` |
| Built-in event payloads | `framework/src/{database,auth,auth_flows,mail,notifications,queue,features}/events.rs` |
| Per-model lifecycle events | macro-generated into each model's `events::` submodule |

## Next

- [Error Model](error-model.md) — `ErrorOccurred` and the 5xx
  conversion path
- [Queues](queues.md) — durable jobs, the crash-tolerant tier;
  `QueuedListener` bridges into this
- [Broadcasting](broadcasting.md) — wire dispatched events to
  WebSocket channels via `EventFacade::broadcast::<E>(hub)`
- [Eloquent](eloquent.md) — model lifecycle events and the
  `Observer<M>` trait
- [Database](database.md) — `DB::listen` and the
  `Database\\QueryExecuted` event
