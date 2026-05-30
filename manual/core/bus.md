---
title: "Bus"
description: "The synchronous command bus — dispatch typed Commands to their registered Handlers, with chain and batch helpers and a record-only test fake"
icon: "diamond-plus"
---

# Bus

The Bus is Suprnova's **synchronous** command dispatcher. You define a
typed `Command` (`{ input, Output type }`), register a `Handler` for it at
boot, and then any code in the process can call `Bus::dispatch(cmd).await`
and get back a `Dispatched<T>` carrying the handler's typed result.

Bus pairs with [`Queue`](./queue.md) — the asynchronous sibling. They are
two intentionally separate facades, not one routing dispatcher:

| If you want…                                          | Use            |
|-------------------------------------------------------|----------------|
| Run the work *now*, in this task, get the result back | `Bus`          |
| Push the work to a worker, retry on failure, durable  | `Queue`        |

The caller picks explicitly. Suprnova does not ship a `ShouldQueue`
marker — on Tokio both paths are non-blocking, so the explicit selection
is clearer and faster than implicit routing.

## Quick Start

Ten lines from command to dispatch:

```rust
use serde::{Deserialize, Serialize};
use suprnova::async_trait;
use suprnova::bus::command::{Command, Handler};
use suprnova::bus::Bus;
use suprnova::error::FrameworkError;

#[derive(Serialize, Deserialize)]
pub struct ChargeCustomer { pub customer_id: i64, pub cents: i64 }

#[async_trait]
impl Command for ChargeCustomer {
    type Output = String; // the charge id we got back
    fn command_name() -> &'static str { "ChargeCustomer" }
}

pub struct ChargeCustomerHandler;

#[async_trait]
impl Handler<ChargeCustomer> for ChargeCustomerHandler {
    async fn handle(&self, cmd: ChargeCustomer) -> Result<String, FrameworkError> {
        Ok(format!("charge-{}-{}", cmd.customer_id, cmd.cents))
    }
}

// At boot (once):
Bus::register::<ChargeCustomer, _>(ChargeCustomerHandler);

// In a request handler:
let charge_id = Bus::dispatch(ChargeCustomer { customer_id: 42, cents: 1999 })
    .await?
    .unwrap_executed();
```

## Defining Commands

A `Command` is any serializable struct with an associated `Output` type
and a unique `command_name()`:

```rust
#[async_trait]
pub trait Command: Serialize + DeserializeOwned + Send + Sync + 'static {
    type Output: Send + 'static;
    fn command_name() -> &'static str;
}
```

The `Output` is what the handler returns. It can be any `Send + 'static`
type, but if you use `Bus::dispatch`/`chain`/`batch` it must also be
`Serialize + DeserializeOwned` because the bus carries values through a
JSON registry (the same encode/decode shape used by `Bus::fake()` and by
the typed handler registry).

`command_name()` should be a stable string unique per concrete `Command`
impl. It shows up in `assert_dispatched`/`assert_dispatched_times` failure
messages and in error returns when no handler is registered.

## Registering Handlers

A `Handler<C>` is a typed async function that takes the command and
returns `Result<C::Output, FrameworkError>`:

```rust
#[async_trait]
pub trait Handler<C: Command>: Send + Sync + 'static {
    async fn handle(&self, cmd: C) -> Result<C::Output, FrameworkError>;
}
```

Call `Bus::register::<C, H>(handler)` once per command type at boot. The
registry is global; re-registering the same `C` overwrites the previous
handler (tests can rely on this to swap implementations).

```rust
Bus::register::<ChargeCustomer, _>(ChargeCustomerHandler);
Bus::register::<RefundCustomer, _>(RefundCustomerHandler);
```

## Dispatching

`Bus::dispatch::<C>(cmd)` runs the registered handler in-process and
returns a `Dispatched<C::Output>` enum:

```rust
pub enum Dispatched<T> {
    Executed(T),  // handler ran, here's the result
    Captured,    // Bus::fake() was active, handler did NOT run
}
```

`Dispatched<T>` has four helpers:

- `.unwrap_executed()` — return the value, panic on `Captured`
- `.executed() -> Option<T>` — convert to `Option`
- `.is_executed()` — bool predicate
- `.is_captured()` — bool predicate

For real-mode call sites, `.unwrap_executed()` is the idiomatic form.

### `Bus::chain` — sequential

`Bus::chain(Vec<C>)` runs commands one at a time, stopping on (and
including) the first error. All commands must be the same type. Returns
`Vec<Result<Dispatched<C::Output>, FrameworkError>>` — one entry per
command attempted.

```rust
let results = Bus::chain(vec![
    ChargeCustomer { customer_id: 1, cents: 100 },
    ChargeCustomer { customer_id: 2, cents: 200 },
    ChargeCustomer { customer_id: 3, cents: 300 },
]).await;

// Collect successful charge ids until the first failure:
let charge_ids: Vec<String> = results
    .into_iter()
    .filter_map(|r| r.ok().and_then(|d| d.executed()))
    .collect();
```

> **Heterogeneous chains** (mixed command types, queued runtime, success
> kicks off the next command) are tracked as a roadmap item — see
> `docs/parity/bus.md` "Open questions".

### `Bus::batch` — concurrent

`Bus::batch(Vec<C>)` runs commands concurrently via `futures::join_all`
and collects results in input order. Same homogeneous-type constraint as
`chain`.

```rust
let results = Bus::batch(vec![
    SendWelcomeEmail { user_id: 1 },
    SendWelcomeEmail { user_id: 2 },
    SendWelcomeEmail { user_id: 3 },
]).await;
```

> **Heterogeneous, persisted batches** (mixed command types, a real
> `Batch` handle with progress + callbacks + lifecycle events +
> `BatchRepository` persistence) are tracked as a roadmap item — see
> `docs/parity/bus.md` "Open questions".

## Testing

Install the fake in a `#[serial]` test (the fake state is process-global
so concurrent tests must serialize):

```rust
use serial_test::serial;
use suprnova::bus::Bus;
use suprnova::bus::testing::{
    assert_dispatched,
    assert_dispatched_times,
    assert_not_dispatched,
    assert_nothing_dispatched,
    install_fake,
};

#[tokio::test]
#[serial]
async fn order_placed_dispatches_charge() {
    let _guard = install_fake();

    place_order(/* … */).await.unwrap();

    assert_dispatched::<ChargeCustomer>(|c| c.customer_id == 42);
    assert_dispatched_times::<ChargeCustomer>(|_| true, 1);
    assert_not_dispatched::<RefundCustomer>(|_| true);
}
```

The fake captures dispatched commands without running their handlers. A
`Bus::dispatch` call returns `Ok(Dispatched::Captured)` (no handler
output) instead of `Executed`. Real errors — encode/decode failures, a
missing registered handler before the fake was installed — still surface
as `Err(_)`.

`install_fake()` returns a `BusFakeGuard`. Drop it (it's RAII) and the
fake is cleared. Inside `#[serial]`-marked tests, the typical idiom is
`let _guard = install_fake();` at the top of the test.

### Assertion surface

| Assertion                                            | Asserts…                                                   |
|------------------------------------------------------|------------------------------------------------------------|
| `assert_dispatched::<C>(pred)`                       | at least one command of type `C` matching `pred`           |
| `assert_not_dispatched::<C>(pred)`                   | zero commands of type `C` matching `pred`                  |
| `assert_dispatched_times::<C>(pred, count)`          | exactly `count` commands of type `C` matching `pred`       |
| `assert_nothing_dispatched()`                        | zero commands of any type dispatched under the active fake |

All four panic with `Bus::fake() must be active` if no fake is installed.
The type-scoped ones panic with `expected … dispatched <command_name> …`
when the count doesn't match. `assert_nothing_dispatched` panics with
`expected no dispatched commands but found <n>`.

## When to use `Queue` instead

Reach for [`Queue`](./queue.md) when you want any of:

- **Durability across restarts.** A queued job survives a process crash
  if the driver is `database` or `redis`.
- **Retries with backoff.** The queue worker applies `Job::max_tries` +
  `Job::backoff` (exponential / fixed / sequence) on each failure.
- **Per-job timeout.** `Job::timeout` + `Job::fail_on_timeout` are honored
  by the worker loop.
- **Delayed execution.** `Queue::later(duration, job)` or
  `Queue::push_later(job, at)`.
- **Dedupe / idempotency.** `Job::unique_id` + `Queue::push_unique`
  gates re-submissions for a configurable TTL.
- **Decoupling the caller from the worker.** Run jobs on a separate
  fleet of `cargo run --bin app -- queue:work` workers.

Reach for `Bus` when you want any of:

- **In-process, run-now.** No serialization across processes.
- **Typed result back to the caller.** `Dispatched<C::Output>` carries
  the handler's typed return value to the call site.
- **Synchronous composition.** A request handler that decomposes work
  into smaller `Command` calls and reads each result in sequence.

A typical app uses both: synchronous request paths dispatch
result-returning operations through `Bus`, and "fire and forget" /
durable work pushes through `Queue`.

## See also

- [Queue](./queue.md) — async sibling, drivers, worker, retry policy
- [Events](./events.md) — pub/sub dispatcher (one event → many listeners)
- `docs/parity/bus.md` — Laravel `Illuminate\Bus` ↔ Suprnova parity audit
  and open questions on net-new subsystems (heterogeneous Batch,
  heterogeneous Chain, DebounceLock, job middleware)
