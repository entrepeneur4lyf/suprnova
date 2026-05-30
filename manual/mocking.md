# Mocking and Fakes

Every external surface in Suprnova ships with an in-process fake that
captures what your code would have sent — mail, notifications, queued
jobs, dispatched commands, fired events, written files, outbound HTTP
calls — and a matching set of assertions you run after the fact. The
shape is always: install the fake, run the code under test, assert
what was captured. This chapter is the consolidated overview; each
subsystem chapter ([Mail](mail.md), [Notifications](notifications.md),
[Queues](queues.md), [Command Bus](bus.md), [Events](events.md),
[File Storage](filesystem.md), [HTTP Client](http-client.md)) covers
its fake in depth.

## The seven fakes

| Surface         | Entry point                                       | Assertion style                       | Parallel safety                                    | Chapter                              |
|-----------------|---------------------------------------------------|---------------------------------------|----------------------------------------------------|--------------------------------------|
| Mail            | `Mail::fake()` → `MailFake` guard                 | methods on the guard                  | needs `#[serial]` — global transport, no serializer | [mail.md](mail.md)                   |
| Notifications   | `Notify::fake()` → `NotifyFakeGuard`              | free functions in `notifications::testing` | guard holds process-wide serializer            | [notifications.md](notifications.md) |
| Queue           | `suprnova::queue::testing::install_fake()`        | free functions in `queue::testing`    | guard holds process-wide serializer                | [queues.md](queues.md)               |
| Bus             | `suprnova::bus::testing::install_fake()`          | free functions in `bus::testing`      | guard holds process-wide serializer                | [bus.md](bus.md)                     |
| Events          | `EventFacade::fake()` → `EventFakeGuard`          | free functions in `events`            | guard holds process-wide serializer                | [events.md](events.md)               |
| Storage         | `Storage::fake()` → `StorageFakeGuard`            | `DiskAssertExt` methods on a disk     | guard holds process-wide serializer                | [filesystem.md](filesystem.md)       |
| HTTP client     | `Http::fake(\|\| async { … }).await`              | `assert_sent` / `assert_not_sent`     | task-local — truly concurrent across tests         | [http-client.md](http-client.md)     |

A few invariants hold across all seven:

- **The fake records, the real backend doesn't run.** Mail isn't sent,
  jobs aren't pushed to the driver, handlers don't run, events skip
  their listeners, HTTP doesn't hit the network, file writes go into
  a memory disk. The captured side carries enough information to
  assert what would have happened.
- **The guard is RAII.** Dropping the guard restores whatever was in
  place before (the previous mail transport, a clean storage registry,
  no recording for events, etc.). Tests don't need a teardown step.
- **The fake doesn't lie about errors.** If your code calls
  `Bus::dispatch` for an unregistered command, the fake still returns
  `Err(_)` — only successful dispatches are captured.

## The shapes, and why they differ

Three patterns recur. Knowing which pattern a fake uses tells you
whether to import a free function, call a method on the guard, or
wrap the test body in a closure.

### Guard-with-methods (Mail)

`Mail::fake()` returns a `MailFake` whose own methods are the
assertions. This is convenient when the asserter is *the* fake — you
already have it bound to a local — but it's the only fake in this
shape:

```rust,ignore
let fake = Mail::fake();
Mail::to("alice@example.org")
    .send(WelcomeEmail { name: "Alice".into() })
    .await?;
fake.assert_sent_count(1);
fake.assert_sent(|m| m.has_to("alice@example.org"));
```

### Guard plus free functions (Notify, Queue, Bus, Events)

The guard is a do-nothing token whose only job is to keep the fake
installed; the assertions live in a `testing` submodule next to the
fake's internals. Import what you need:

```rust,ignore
use suprnova::queue::testing::{install_fake, assert_pushed, pushed};

let _guard = install_fake();
schedule_welcome_email(user_id).await?;
assert_pushed::<WelcomeJob>(|j| j.user_id == user_id);
```

This is the most common shape because it generalises cleanly across
types — every assertion is generic over `J: Job` / `C: Command` /
`E: Event` instead of being baked into a guard type. The trade-off
is one extra import.

### Scope-with-closure (HTTP)

`Http::fake` is the odd one out. Outbound HTTP runs on whatever Tokio
task happens to be alive, so the fake state lives in a
`tokio::task_local!`. You can't install it once and let it ride —
you have to wrap the body that calls the client:

```rust,ignore
use suprnova::{Http, fake_response, assert_sent};

Http::fake(|| async {
    fake_response("POST", "/api/users", 201, serde_json::json!({"id": 1}));

    let resp = Http::post("https://example.com/api/users")
        .json(&serde_json::json!({"name": "Ada"}))
        .send()
        .await?;

    assert_eq!(resp.status(), 201);
    assert_sent(|r| r.method == "POST" && r.url.contains("/api/users"));
})
.await;
```

The payoff: every other fake holds a process-wide serializer so
parallel tests run one-at-a-time, but `Http::fake` is truly
concurrent — every test gets its own task-local recorder and they
never collide.

### Storage's extension trait

`Storage::fake()` returns a guard *and* a default in-memory disk, but
its assertions hang off the disk itself through the `DiskAssertExt`
extension trait:

```rust,ignore
use suprnova::{Storage, DiskExt};
use suprnova::filesystem::testing::DiskAssertExt;

let _guard = Storage::fake();
let disk = Storage::disk("default")?;

disk.put("invoices/42.pdf", b"...").await?;
disk.assert_exists("invoices/42.pdf").await;
disk.assert_count("invoices/", 1, false).await;
```

The extension trait is gated on `#[cfg(any(test, feature = "testing"))]`
so production code can't accidentally call `disk.assert_exists(…)`.

## Parallel safety, in one paragraph

Six of the seven fakes guard a process-global static. Each one's
guard, on construction, takes a dedicated `FAKE_SERIAL`
`std::sync::Mutex` and holds it until drop. The effect is that any
two `#[tokio::test]`s that install the same fake run serialized
under one process — no need for `#[serial]` from the
[serial_test](https://crates.io/crates/serial_test) crate. **Mail
is the exception**: the `MailFake` guard swaps the global
`TRANSPORT` without taking a serializer, so concurrent `Mail::fake()`
tests *would* clobber each other. Mark them `#[serial]`. **`Http::fake`
is also an exception**: it's task-local, not process-global, so tests
genuinely run in parallel and never need `#[serial]`.

If you interleave real-dispatch with fake-dispatch for the same
surface inside one test binary, the real path doesn't take the
serializer, so it can race a parallel faked test. Mark the
real-dispatch tests `#[serial]` in that case — the per-chapter docs
call this out where it applies (see [Command Bus](bus.md) for the
canonical example).

## Mail — `Mail::fake()`

```rust,ignore
use serial_test::serial;
use suprnova::mail::{Mail, Address};

#[tokio::test]
#[serial]
async fn welcome_email_is_sent() {
    let fake = Mail::fake();

    register_user("alice@example.org").await.unwrap();

    fake.assert_sent_count(1);
    fake.assert_sent(|m| m.has_to("alice@example.org"));
    fake.assert_sent(|m| m.subject.starts_with("Welcome"));
    fake.assert_not_sent_to("eve@example.org");
}
```

| Assertion                                  | Asserts…                                            |
|--------------------------------------------|-----------------------------------------------------|
| `fake.assert_sent(\|m\| pred)`             | at least one captured message matches               |
| `fake.assert_sent_to("…")`                 | at least one captured message was routed to email   |
| `fake.assert_not_sent(\|m\| pred)`         | no captured message matches                         |
| `fake.assert_not_sent_to("…")`             | no captured message went to email                   |
| `fake.assert_sent_count(n)`                | exactly `n` captured messages                       |
| `fake.assert_nothing_sent()`               | nothing was captured                                |
| `fake.assert_queued("MailableName")`       | at least one queued mailable of this name           |
| `fake.assert_queued_with(name, \|q\| …)`   | a queued mailable matches the predicate             |
| `fake.assert_queued_to("…")`               | a queued mailable was routed to email               |
| `fake.assert_not_queued("MailableName")`   | no queued mailable of this name                     |
| `fake.assert_queued_count(n)`              | exactly `n` queued mailables                        |
| `fake.assert_nothing_queued()`             | nothing was queued                                  |
| `fake.assert_outgoing_count(n)`            | sent + queued totals `n`                            |
| `fake.assert_nothing_outgoing()`           | nothing was sent and nothing was queued             |

`fake.captured()`, `fake.queued()`, `fake.sent(pred)`, `fake.sent_to(…)`,
`fake.queued_named(…)`, and `fake.queued_to(…)` return the matching
data so you can build custom assertions. See [Mail](mail.md) for the
full surface, including how `Mail::queue` is mirrored into the fake
even when `Queue::fake` isn't installed.

## Notifications — `Notify::fake()`

```rust,ignore
use suprnova::notifications::{Notify, testing};

#[tokio::test]
async fn order_shipped_notifies_customer() {
    let _guard = Notify::fake();

    ship_order(order_id).await.unwrap();

    testing::assert_sent_to("alice@example.org", "OrderShipped");
    testing::assert_sent_to_on("alice@example.org", "mail", "OrderShipped");
    testing::assert_sent_times("OrderShipped", 1);
}
```

| Assertion                                            | Asserts…                                          |
|------------------------------------------------------|---------------------------------------------------|
| `assert_sent(\|r\| pred)`                            | at least one dispatched notification matches      |
| `assert_sent_to(route, "Name")`                      | named notification went to this per-channel route |
| `assert_sent_to_on(route, channel, "Name")`          | dispatched on this channel to this route          |
| `assert_sent_named("Name")`                          | named notification dispatched on any channel      |
| `assert_sent_times("Name", n)`                       | exactly `n` of the named notification             |
| `assert_nothing_sent()`                              | no notifications dispatched                       |
| `assert_count(n)`                                    | exactly `n` total across all types and channels   |
| `assert_nothing_sent_to(route)`                      | nothing dispatched to this route                  |

`testing::recorded()` returns every `FakeRecord` (notification name,
channel, route, JSON data) for finer-grained assertions. Notification
recipients are keyed on the per-channel `route_for` value, so
`assert_sent_to` takes the route string (an email address for `"mail"`,
the id-as-string for `"database"`, …) — see [Notifications](notifications.md)
for the routing model.

## Queue — `queue::testing::install_fake()`

```rust,ignore
use suprnova::Queue;
use suprnova::queue::testing::{
    install_fake, assert_pushed, assert_pushed_later, pushed,
};

#[tokio::test]
async fn order_placed_enqueues_charge() {
    let _guard = install_fake();

    place_order(42).await.unwrap();

    assert_pushed::<ChargeCustomerJob>(|j| j.order_id == 42);
}
```

| Assertion                                      | Asserts…                                                       |
|------------------------------------------------|----------------------------------------------------------------|
| `assert_pushed::<J>(\|j\| pred)`               | at least one push of `J` matches                               |
| `assert_pushed_later::<J>(\|j, at\| pred)`     | a push of `J` was scheduled at `at` (delayed dispatch)         |

The data side returns the typed jobs themselves:

- `pushed::<J>() -> Vec<J>` — every captured push of `J`
- `pushed_with_available_at::<J>() -> Vec<(J, DateTime<Utc>)>` — same,
  with each job's scheduled timestamp

Every `Queue::push`, `Queue::push_later`, `Queue::later`,
`Queue::push_unique*`, and the chain/batch dispatchers all funnel
into the same recorder. See [Queues](queues.md) for `push_unique`
semantics under the fake (it always records and reports "pushed").

## Bus — `bus::testing::install_fake()`

```rust,ignore
use suprnova::Bus;
use suprnova::bus::testing::{
    install_fake, assert_dispatched, assert_dispatched_times,
    assert_not_dispatched, assert_nothing_dispatched,
};

#[tokio::test]
async fn order_placed_dispatches_charge() {
    let _guard = install_fake();

    place_order(42).await.unwrap();

    assert_dispatched::<ChargeCustomer>(|c| c.customer_id == 42);
    assert_dispatched_times::<ChargeCustomer>(|_| true, 1);
    assert_not_dispatched::<RefundCustomer>(|_| true);
}
```

| Assertion                                           | Asserts…                                                      |
|-----------------------------------------------------|---------------------------------------------------------------|
| `assert_dispatched::<C>(\|c\| pred)`                | at least one dispatched command of `C` matches                |
| `assert_not_dispatched::<C>(\|c\| pred)`            | no dispatched command of `C` matches                          |
| `assert_dispatched_times::<C>(\|c\| pred, n)`       | exactly `n` dispatched commands of `C` match                  |
| `assert_nothing_dispatched()`                       | zero commands of any type dispatched under the active fake    |

Under the fake, `Bus::dispatch` returns `Ok(Dispatched::Captured)`
instead of running the handler. Real failures — encode/decode
errors, no handler registered before the fake was installed — still
surface as `Err(_)`. See [Command Bus](bus.md).

## Events — `EventFacade::fake()`

```rust,ignore
use suprnova::EventFacade;
use suprnova::events::{
    assert_dispatched, assert_dispatched_once, assert_dispatched_times,
    assert_not_dispatched, assert_nothing_dispatched, dispatched,
    dispatched_count, dispatched_events, has_dispatched,
};

#[tokio::test]
async fn registration_dispatches_welcome_event() {
    let _guard = EventFacade::fake();

    register_user("ada@example.com").await.unwrap();

    assert_dispatched_once::<UserRegistered>();
    assert_dispatched::<UserRegistered>(|e| e.email == "ada@example.com");
}
```

| Assertion                              | Asserts…                                          |
|----------------------------------------|---------------------------------------------------|
| `assert_dispatched::<E>(\|e\| pred)`   | at least one dispatched `E` matches               |
| `assert_dispatched_once::<E>()`        | exactly one `E` was dispatched                    |
| `assert_dispatched_times::<E>(n)`      | exactly `n` of `E` were dispatched                |
| `assert_not_dispatched::<E>(\|e\| ..)` | no matching `E` was dispatched                    |
| `assert_nothing_dispatched()`          | no events of any type dispatched                  |
| `assert_listening::<E, L>()`           | listener `L` is registered for `E`                |
| `has_dispatched::<E>()`                | `bool`: any `E` recorded                          |
| `dispatched::<E>(\|e\| pred)`          | `Vec<E>` clones of matching events                |
| `dispatched_count::<E>(\|e\| pred)`    | count of matching events                          |
| `dispatched_events()`                  | `HashMap<&'static str, usize>` of all dispatches  |

Two variants narrow what's faked:

```rust,ignore
// Only fake these — everything else dispatches normally.
let _guard = EventFacade::fake_only(&["UserRegistered", "UserDeleted"]);

// Fake every event EXCEPT these.
let _guard = EventFacade::fake_except(&["TelemetryEvent"]);
```

And one variant suppresses without recording:

```rust,ignore
EventFacade::muted(async {
    // No listeners fire, no events recorded.
    run_bulk_import().await;
})
.await;
```

`muted` does NOT acquire the serializer, so muted scopes can run in
parallel. See [Events](events.md) for the full machinery, including
`assert_listening` (which observes listener registrations that happen
*inside* the fake's scope only).

## Storage — `Storage::fake()`

```rust,ignore
use suprnova::{Storage, DiskExt};
use suprnova::filesystem::testing::DiskAssertExt;

#[tokio::test]
async fn invoice_upload_persists() {
    let _guard = Storage::fake();
    let disk = Storage::disk("default").unwrap();

    upload_invoice(b"%PDF-1.7 …").await.unwrap();

    disk.assert_exists("invoices/2026/05/30/inv-00042.pdf").await;
    disk.assert_contents("invoices/2026/05/30/inv-00042.pdf", b"%PDF-1.7 …").await;
}
```

The guard pre-registers a `"default"` in-memory disk, so trivial
tests don't need any disk setup. Register additional disks under
custom names with `Storage::register_memory("audit_logs")` from
inside the test if the code under test reaches for a non-default
disk.

| Assertion                                        | Asserts…                                          |
|--------------------------------------------------|---------------------------------------------------|
| `disk.assert_exists(path).await`                 | the path exists                                   |
| `disk.assert_contents(path, &expected).await`    | the file matches `expected` byte-for-byte         |
| `disk.assert_missing(path).await`                | the path does not exist                           |
| `disk.assert_count(dir, n, recursive).await`     | `dir` contains exactly `n` entries                |
| `disk.assert_directory_empty(dir).await`         | `dir` has no entries (recursive)                  |

All five panic on mismatch with the disk path in the message. See
[File Storage](filesystem.md) for the `Storage` facade itself and
the driver story (memory / fs / s3 / azblob / gcs).

## HTTP client — `Http::fake`

```rust,ignore
use suprnova::{Http, fake_response, assert_sent, assert_not_sent};

#[tokio::test]
async fn payment_webhook_is_acked() {
    Http::fake(|| async {
        fake_response("POST", "/v1/charges", 201, serde_json::json!({
            "id": "ch_42",
            "status": "succeeded",
        }));

        let result = charge_card(amount_cents).await;

        assert!(result.is_ok());
        assert_sent(|r| r.method == "POST" && r.url.contains("/v1/charges"));
        assert_not_sent(|r| r.method == "DELETE");
    })
    .await;
}
```

`fake_response(method, url_substring, status, body)` queues one
canned response. Method `"*"` matches any method. Each canned entry
is consumed on the first matching request; subsequent matching
requests either fall through to the next canned entry or return an
empty `200 {}`.

| Helper                                       | Purpose                                                   |
|----------------------------------------------|-----------------------------------------------------------|
| `Http::fake(\|\| async { … }).await`         | install the task-local fake scope                         |
| `fake_response(method, url_substring, …)`    | queue a canned response                                   |
| `assert_sent(\|r\| pred)`                    | assert at least one recorded request matches              |
| `assert_not_sent(\|r\| pred)`                | assert no recorded request matches                        |

### Spawned tasks don't inherit the fake by default

`tokio::spawn` doesn't carry task-locals into the spawned future, so
work that escapes the parent task escapes the fake too. Two tools
handle this:

```rust,ignore
// Belt-and-suspenders: turn every unfaked outbound call into a hard error.
let _guard = suprnova::FailOnRealCallsGuard::install();

Http::fake(|| async {
    fake_response("GET", "/child", 204, serde_json::json!({}));

    // Explicit opt-in: this child sees the parent's fake state.
    let handle = Http::spawn_with_fake_inheritance(async {
        Http::get("https://child.test").send().await
    });

    let response = handle.await.unwrap().unwrap();
    assert_eq!(response.status(), 204);
})
.await;
```

`FailOnRealCallsGuard` is RAII — install it at the top of a test and
any outbound call that doesn't hit an active fake errors out instead
of touching the network. `Http::spawn_with_fake_inheritance` is the
explicit opt-in for tasks that should share the parent's fake state.
See [HTTP Client](http-client.md) for the full discussion.

## Broadcasting

WebSocket broadcasting has a parallel test fixture, but its shape
differs enough that it lives in its own chapter:
`RecordingBroadcastHub` is a real `BroadcastHub` that records every
published envelope while still delivering to live subscribers. Bind
it in place of `InMemoryBroadcastHub` and call `hub.broadcasts()` /
`hub.assert_broadcast(channel, event)`. See
[Broadcasting](broadcasting.md) for the broadcasting model and the
recording-hub usage.

## Where each fake lives

| Surface       | Source                                | Facade re-export                             |
|---------------|---------------------------------------|----------------------------------------------|
| Mail          | `framework/src/mail/mod.rs`           | `suprnova::{Mail, MailFake}`                 |
| Notifications | `framework/src/notifications/testing.rs` | `suprnova::{Notify, NotifyFakeGuard}` + `suprnova::notifications::testing::*` |
| Queue         | `framework/src/queue/testing.rs`      | `suprnova::queue::testing::*`                |
| Bus           | `framework/src/bus/testing.rs`        | `suprnova::bus::testing::*`                  |
| Events        | `framework/src/events/testing.rs`     | `suprnova::{EventFacade, EventFakeGuard}` + `suprnova::events::*` |
| Storage       | `framework/src/filesystem/testing.rs` | `suprnova::{Storage, DiskExt}` + `suprnova::filesystem::testing::DiskAssertExt` |
| HTTP          | `framework/src/http_client/fake.rs`   | `suprnova::{Http, fake_response, assert_sent, assert_not_sent, FailOnRealCallsGuard, RecordedRequest}` |

The `testing` and `fake` modules are gated behind a Cargo feature
named `testing`. It's in the default feature set, so any test that
depends on `suprnova` picks the helpers up for free. The hooks
themselves are `#[doc(hidden)]` where they could be reached
accidentally from application code; the load-bearing safeguard is
`Server::from_config`'s `APP_KEY` validation, which runs on every
boot regardless of which test helpers are compiled in. See
[Testing](testing.md) for the production-build story.

## Why these shapes, not one shape

A single uniform shape would be neater on the page and worse in
practice. Each shape exists because the underlying state has
different concurrency semantics:

- **Mail's** transport is a global `Arc<dyn MailTransport>` swapped
  by the guard. Method assertions on the returned guard tie the
  asserter to the specific install, which makes it impossible to
  call assertions when no fake is active.
- **Notify / Queue / Bus / Events** assert on heterogeneous typed
  payloads — every assertion is generic over the event/job/command
  type. Free functions in a `testing` module compose with type
  parameters more cleanly than a hand-written method set on a guard.
- **Storage** assertions are per-disk, not per-fake — the same
  `disk.assert_exists(…)` works against a faked memory disk or a
  real `s3` disk in an integration suite. Putting them on the disk
  via an extension trait keeps that symmetry.
- **HTTP** has to follow tasks, not the calling stack. `Http::fake`
  is the only fake whose scope can't be expressed as a guard —
  spawn semantics force a closure.

If you ever find yourself reaching for a helper that doesn't exist,
read the relevant chapter; the public testing surface is documented
exhaustively per subsystem.

## Next

- [Testing](testing.md) — the `#[suprnova_test]` macro, `TestDatabase`,
  `expect!`, and `TestContainer::fake`
- [HTTP Tests](http-tests.md) — driving `handle_request` directly
  without opening a socket
- [Database Tests](database-testing.md) — the per-test in-memory
  database story
- [Service Container](container.md) — `TestContainer::fake` for
  swapping injected services
