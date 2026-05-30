---
title: "Events"
description: "Typed in-process pub/sub — dispatch events, listen with typed listeners, defer / push / fake / mute, parity-matched to Laravel's Event facade"
icon: "radio"
---

# Events

Events are Suprnova's typed in-process pub/sub. A controller fires
`UserRegistered { user_id }`; one listener emails the user, another
writes an audit row, a third publishes a broadcast. All three see the
same payload, run in registration order, and don't know each other
exist.

The user-facing surface lives on the `EventFacade` (re-exported from
`suprnova::` as `EventFacade`, also available under the shorter
`Event` alias in the prelude — but we use `EventFacade` here so it
doesn't collide with the `Event` *trait*). Behind it is a single
process-global `EventDispatcher` — held in a `OnceLock`, registered
listeners survive the request that registered them, and dispatches
either run inline (synchronous) or spawn into a bounded retrying task
set (queued).

## The basics

```rust,ignore
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
        Ok(())
    }
}

// In bootstrap.rs:
EventFacade::listen::<UserRegistered, SendWelcomeEmail>(Arc::new(SendWelcomeEmail)).await;

// In a controller:
EventFacade::dispatch(UserRegistered { user_id: 42 }).await?;
```

`Event: Send + Sync + Clone + 'static + Debug` so a payload can cross
task boundaries (queued listeners) and the dispatcher can log it.
`Listener<E>: Send + Sync + 'static` so it can outlive the registration
call.

## Dispatch modes

| Method                          | Semantics                                         |
| ------------------------------- | ------------------------------------------------- |
| `dispatch(event)`               | Sync, fail-fast — first `Err` aborts the chain    |
| `dispatch_best_effort(event)`   | Sync, run-them-all — returns the first `Err`      |
| `dispatch(event)` *(queued)*    | Spawns a bounded task per listener with retries   |

Pick `dispatch` (fail-fast) when a downstream side effect MUST observe
a successful upstream; pick `dispatch_best_effort` for fan-out where
one failing listener should not silence the rest.

Set `fn queued() -> bool { true }` on the event trait impl and every
listener runs as its own task (bounded by a process-wide semaphore;
override via `EVENT_MAX_CONCURRENCY` env var or
`EventDispatcher::with_concurrency(n)`).

## Inspecting registrations

```rust,ignore
if EventFacade::has_listeners::<UserRegistered>() {
    EventFacade::dispatch(UserRegistered { user_id: 42 }).await?;
}
```

`has_listeners::<E>()` is the typed mirror of Laravel's
`Event::hasListeners($eventName)`. Returns `false` when the registry
lock is poisoned (fail-safe).

`forget::<E>()` drops every listener registered for that event type and
returns the count removed. Mirrors Laravel's `Event::forget($event)`.
Production code rarely needs it — listener registration is normally
bootstrap-once — but hot-swap and test code reach for it.

## Push / flush

`push(event)` captures the event in a per-event-name bucket without
firing it. `flush::<E>()` drains that bucket and dispatches every
queued event in capture order. Mirrors Laravel's `Event::push` /
`Event::flush` pair.

```rust,ignore
// Inside a request handler that does work in two phases:
EventFacade::push(UserRegistered { user_id: 42 }).await;
// … rendering, validation, more work …
EventFacade::flush::<UserRegistered>().await?;
```

Pushed events ignore the `defer` scope — they are already explicitly
deferred. `forget_pushed()` drops every pushed event without
dispatching, returning the count dropped. Mirrors
`Event::forgetPushed()`.

## defer — buffer every dispatch inside a callback

`defer(only, async { … })` runs the callback with a task-local buffer
in scope. Every `dispatch` / `dispatch_best_effort` call made inside
the callback (on ANY dispatcher in this task) is captured and replayed
after the callback returns. Mirrors Laravel's
`Event::defer($callback, ?$events)`.

```rust,ignore
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

Pass `Some(&["EventOne", "EventTwo"])` to defer ONLY those event names;
everything else dispatches inline as usual. A callback error short-
circuits — buffered events are dropped, the error propagates.

The defer buffer is per-Tokio-task, so two concurrent `defer` calls
don't stomp on each other's state.

## Subscribers — bundle related registrations

Laravel's "subscribe" pattern: instead of registering ten listeners
individually, a single struct's `subscribe` method registers them all.

```rust,ignore
use suprnova::{EventFacade, Subscriber, EventDispatcher, async_trait};
use std::sync::Arc;

pub struct UserEventSubscriber {
    db: Arc<crate::Db>,
}

#[async_trait]
impl Subscriber for UserEventSubscriber {
    async fn subscribe(self: Arc<Self>, d: &EventDispatcher) {
        d.listen::<UserRegistered, _>(Arc::new(SendWelcomeEmail::new(self.db.clone()))).await;
        d.listen::<UserDeleted, _>(Arc::new(CleanupUserData::new(self.db.clone()))).await;
        d.listen::<UserPromoted, _>(Arc::new(NotifyAdmins::new(self.db.clone()))).await;
    }
}

// In bootstrap.rs — one line per subscriber instead of three per listener:
EventFacade::subscribe(Arc::new(UserEventSubscriber { db: db.clone() })).await;
```

`subscribe` takes `Arc<S>` so listeners that need to share state with
the subscriber can clone the `Arc` and capture it.

## Queued listeners — durable vs in-process

Two distinct "queued" tiers, and the naming matters:

| Need                                                | Reach for                                          |
| --------------------------------------------------- | -------------------------------------------------- |
| Listener should run off-task, OK to lose on crash   | `Event::queued() = true` on the event trait        |
| Listener work MUST survive a crash + restart        | `QueuedListener` (bridges event → durable job)     |

`Event::queued() = true` makes the dispatcher spawn each listener as
its own Tokio task, bounded by a process semaphore, with bounded retry
(3 attempts, 100ms→2s jittered backoff). The work runs on this
process; a crash drops in-flight listeners.

`QueuedListener<E, J>` is a stock listener implementation that builds
a [`Job`](./queue.md) from each event and pushes it on the durable
queue. The event still fires synchronously and unbounded; the listener
just enqueues — which is fast — so request latency stays low. The job
itself survives the crash.

```rust,ignore
use suprnova::{EventFacade, QueuedListener};
use std::sync::Arc;

EventFacade::listen::<UserRegistered, _>(Arc::new(
    QueuedListener::<UserRegistered, SendWelcomeEmailJob>::new(|e| SendWelcomeEmailJob {
        user_id: e.user_id,
    }),
))
.await;
```

## Drain on shutdown

Queued listeners spawn into a `JoinSet` tracked by the dispatcher. The
server's graceful-shutdown sequence calls
`EventFacade::drain_queued(timeout)` to wait for them.

```rust,ignore
let still_running = EventFacade::drain_queued(Duration::from_secs(30)).await;
if still_running > 0 {
    tracing::warn!(still_running, "queued listeners abandoned at shutdown");
}
```

Drain returns the count still running when the deadline elapsed (`0` =
fully drained). Stragglers past the deadline are aborted so shutdown
cannot hang.

## Built-in events

| Event           | Emitted by                          | Notes                                         |
| --------------- | ----------------------------------- | --------------------------------------------- |
| `ErrorOccurred` | `FrameworkError` → 5xx conversion   | best-effort, spawned, drop if no runtime live |

`ErrorOccurred` is the hook for shipping 5xx exceptions to Sentry,
Datadog, Slack, etc. The dispatch is `dispatch_best_effort` so a
broken Sentry listener cannot silence the rest, and it's spawned —
response conversion never blocks on it.

## Testing — `EventFacade::fake()`

`Event::fake()` swaps the global dispatcher with a recorder. Dispatched
events go into the recording instead of running listeners.

```rust,ignore
use suprnova::events::{
    assert_dispatched, assert_dispatched_once, assert_dispatched_times,
    assert_nothing_dispatched, has_dispatched, dispatched, dispatched_events,
    EventFacade,
};

#[tokio::test]
async fn registration_dispatches_welcome_event() {
    let _guard = EventFacade::fake();

    register_user("ada@example.com").await.unwrap();

    assert_dispatched_once::<UserRegistered>();
    assert_dispatched::<UserRegistered>(|e| e.email == "ada@example.com");
}
```

| Helper                              | Asserts…                                          |
| ----------------------------------- | ------------------------------------------------- |
| `assert_dispatched::<E>(pred)`      | at least one matching `E` was dispatched          |
| `assert_dispatched_once::<E>()`     | exactly one `E` was dispatched                    |
| `assert_dispatched_times::<E>(n)`   | exactly `n` of `E` were dispatched                |
| `assert_not_dispatched::<E>(pred)`  | no matching `E` was dispatched                    |
| `assert_nothing_dispatched()`       | NO events of any type were dispatched             |
| `assert_listening::<E, L>()`        | a listener `L` was registered for `E`             |
| `has_dispatched::<E>()`             | bool: any `E` recorded                            |
| `dispatched::<E>(pred)`             | `Vec<E>` clones of matching events                |
| `dispatched_count::<E>(pred)`       | count of matching events                          |
| `dispatched_events()`               | `HashMap<&'static str, usize>` of all dispatches  |

The fake holds a process-wide serializer for the duration of the guard,
so parallel `#[tokio::test]`s using it run one at a time. Tests no
longer need their own `serial_test` mutex.

### Selective faking

```rust,ignore
// Only fake these events; everything else dispatches normally.
let _guard = EventFacade::fake_only(&["UserRegistered", "UserDeleted"]);

// Fake every event EXCEPT these.
let _guard = EventFacade::fake_except(&["TelemetryEvent"]);
```

Mirrors Laravel's `Event::fake([…])` (the `eventsToFake` arg) and
`EventFake::except($events)`.

### Mute — discard events without recording

`EventFacade::muted(async { … })` runs the callback with a task-local
"silent dispatcher" flag set; every event dispatched inside is
discarded without recording or invoking listeners. The Suprnova
analogue of Laravel's `NullDispatcher`, scoped to a callback.

```rust,ignore
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

```rust,ignore
#[tokio::test]
async fn bootstrap_wires_welcome_listener() {
    let _guard = EventFacade::fake();
    bootstrap::register_listeners().await;
    assert_listening::<UserRegistered, SendWelcomeEmail>();
}
```

The fake observes registrations via the dispatcher's `listen`
method, so the registration must happen INSIDE the fake's scope —
listeners registered before `Event::fake()` are NOT seen by
`assert_listening`.

## Laravel parity reference

Every Laravel 13 `Event` facade and `EventFake` method that has a
typed-Rust equivalent is shipped under the closest matching name.
Methods Laravel exposes that don't fit typed Rust are omitted with a
short note.

| Laravel                                | Suprnova                                    |
| -------------------------------------- | ------------------------------------------- |
| `Event::dispatch($event)`              | `EventFacade::dispatch(event).await`        |
| `Event::dispatch($event)` (halt arg)   | use `dispatch` (fail-fast on `Err`)         |
| `Event::until($event)`                 | `dispatch` (typed: first `Err` halts)       |
| `Event::listen($event, $listener)`     | `EventFacade::listen::<E, L>(Arc::new(L))`  |
| `Event::hasListeners($name)`           | `EventFacade::has_listeners::<E>()`         |
| `Event::forget($event)`                | `EventFacade::forget::<E>()`                |
| `Event::push($event)`                  | `EventFacade::push(event).await`            |
| `Event::flush($event)`                 | `EventFacade::flush::<E>().await`           |
| `Event::forgetPushed()`                | `EventFacade::forget_pushed().await`        |
| `Event::defer($callback, ?$events)`    | `EventFacade::defer(only, async {…}).await` |
| `Event::subscribe($subscriber)`        | `EventFacade::subscribe(Arc::new(S)).await` |
| `Event::fake()`                        | `EventFacade::fake()` (guard)               |
| `Event::fake([$names])`                | `EventFacade::fake_only(&["…"])`            |
| `EventFake::except($names)`            | `EventFacade::fake_except(&["…"])`          |
| `EventFake::assertDispatched`          | `assert_dispatched`                         |
| `EventFake::assertDispatchedOnce`      | `assert_dispatched_once`                    |
| `EventFake::assertDispatchedTimes`     | `assert_dispatched_times`                   |
| `EventFake::assertNotDispatched`       | `assert_not_dispatched`                     |
| `EventFake::assertNothingDispatched`   | `assert_nothing_dispatched`                 |
| `EventFake::assertListening`           | `assert_listening`                          |
| `EventFake::hasDispatched`             | `has_dispatched`                            |
| `EventFake::dispatched`                | `dispatched` (returns `Vec<E>`)             |
| `EventFake::dispatchedEvents`          | `dispatched_events` (name → count map)      |
| `NullDispatcher`                       | `EventFacade::muted(async {…}).await`       |
| `Event::wildcards` (`User.*` patterns) | not shipped — use typed listeners or the cancellable `Observer` trait on Eloquent for the per-model "before save" / "after save" hook surface |
| `Event::subscribe` (string subscriber) | use the typed `Subscriber` trait            |

## See also

- [`Queue`](./queue.md) — durable jobs, the crash-tolerant tier
- [`Broadcasting`](./broadcasting.md) — bridge events to WebSocket
  channels (`EventFacade::broadcast::<E>(hub)`)
- [Model lifecycle events](./eloquent.md) — per-model `Created`,
  `Updating`, etc. with cancellable listeners via the `Observer` trait
