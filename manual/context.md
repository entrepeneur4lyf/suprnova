# Context

`Context` is Suprnova's per-request key/value bag. It's where you stash
data you want every downstream caller in the same request to see — a
request id, a tenant slug, a user role, an audit trail — without
threading the value through every function signature. It's the Suprnova
equivalent of Laravel's `Context` facade.

```rust
use suprnova::Context;

Context::add("tenant_id", "acme");
Context::push("breadcrumbs", "checkout/start");
Context::hidden_add("api_key", secret);

let tenant: Option<String> = Context::get("tenant_id");
let page: Option<String> = Context::query_param("page");
```

Reach for it when:

- A log line, queued job, or broadcast message needs request-scoped
  metadata (tenant id, correlation id, user role)
- A deeply-nested helper needs a value the handler already has, but
  the call chain shouldn't carry a parameter through every layer
- You want to read the current request's query string (`?page=3`,
  `?cursor=…`) from code that isn't a handler

`Context` is **not** for cross-request state. It's bound to the
current Tokio task and disappears when the request ends. For things
that outlive a request, use the [Service Container](container.md) or
[Cache](cache.md).

## The two bags

Every active `Context` scope carries two key/value maps and one extra
slot:

| Bag | Read with | Appears in `Context::all()` |
|---|---|---|
| **Visible** | `Context::get` | Yes |
| **Hidden** | `Context::hidden_get` | No |
| **Query** | `Context::query_param` | No (separate snapshot of the URL's `?key=value` pairs) |

The split between visible and hidden is the whole point of having
two bags: log serialisers that dump `Context::all()` into structured
output won't leak data you intentionally hide. Put audit metadata in
the visible bag; put API keys, OAuth bearer tokens, and PII you don't
want in logs in the hidden bag.

The query bag is populated automatically by the framework's request
middleware from the URL's query string (see
[Pagination reads query params](#pagination-reads-query-params)
below). You usually only read it, never write it.

## The active scope

A `Context` scope is installed by the framework on every incoming
HTTP request. Inside a handler, middleware, model observer, event
listener, or anything else reachable from the request task, the
scope is live and `Context::*` reads and writes work without
ceremony.

Outside a scope — early-boot code, a bare `tokio::spawn` that doesn't
inherit context, a unit test that doesn't install one — every
mutation is a **silent no-op** and every read returns `None`. The
contract is: no panic, ever, regardless of where you call from.

```rust
// In a handler — scope is active, everything works:
Context::add("user_id", 42i64);
let id: Option<i64> = Context::get("user_id");
assert_eq!(id, Some(42));

// Outside a scope — silent no-op + None:
Context::add("user_id", 42i64);            // discarded
let id: Option<i64> = Context::get("user_id");
assert_eq!(id, None);
```

The no-panic contract is deliberate. Library code that touches
`Context` (a custom log subscriber, an SDK extension) shouldn't need
to know whether it's running inside a request or at boot — it should
just call `Context::get` and treat `None` as "not available right now".

### Observability for silent operations

A truly silent no-op would hide bugs (middleware out of order,
context not propagated into a spawned task, accidental boot-time
read). The framework's mutating operations stay no-panic but emit a
`tracing::trace!` event on the `suprnova::context` target whenever
they discard:

```text
TRACE suprnova::context: Context mutation discarded: no active scope on this task op="add"
TRACE suprnova::context: Context mutation discarded: value failed to serialize op="push" key="bad"
TRACE suprnova::context: Context read returned None: value present but did not deserialize op="get" key="user_id" expected="String"
```

Three classes of event:

| Event | When it fires |
|---|---|
| `mutation discarded: no active scope` | `add`, `push`, `hidden_add`, `forget` called outside any scope |
| `mutation discarded: value failed to serialize` | `add`/`push`/`hidden_add` value's `Serialize` impl errored |
| `read returned None: value present but did not deserialize` | `get`/`hidden_get` found the key but the stored JSON doesn't match the requested `T` |

Plain absence — `get` on a key that was never set — stays silent so
"is this set?" probes don't flood logs. Enable
`RUST_LOG=suprnova::context=trace` when you suspect a propagation
bug; the silent no-op path becomes visible without changing how
production code behaves.

## Adding values

### `Context::add` — replace at a key

```rust
use suprnova::Context;

Context::add("user_id", 42i64);
Context::add("tenant", "acme");
Context::add("plan", PlanTier::Pro);     // any Serialize value
```

The key is `Into<String>`; the value is any `Serialize` type. The
value is converted to `serde_json::Value` once at write time and
stored that way. Subsequent `add` on the same key replaces.

### `Context::push` — append to a stack

```rust
Context::push("trail", "home");
Context::push("trail", "settings");
Context::push("trail", "billing");

let trail: Vec<String> = Context::get("trail").unwrap();
assert_eq!(trail, vec!["home", "settings", "billing"]);
```

`push` initialises an empty array on the first call and appends on
subsequent calls. If a scalar already exists at the key, it's
converted to a `[scalar, new_value]` array — `push` is forgiving
about prior `add`s on the same key.

### `Context::hidden_add` — write to the hidden bag

```rust
Context::hidden_add("api_key", os_env_secret);
Context::hidden_add("oauth_bearer", token);

// Visible bag dump (e.g. a JSON log emitter) doesn't see them:
let all = Context::all();
assert!(!all.contains_key("api_key"));

// But you can still read them deliberately:
let key: Option<String> = Context::hidden_get("api_key");
```

The hidden bag is keyed independently from the visible bag — a
`hidden_add("user_id", 99)` and an `add("user_id", "alice")` coexist
without collision. `Context::forget(key)` removes from both bags in
one call.

## Reading values

### `Context::get` — typed read from the visible bag

```rust
use suprnova::Context;

let user_id: Option<i64>       = Context::get("user_id");
let tenant:  Option<String>    = Context::get("tenant");
let trail:   Option<Vec<String>> = Context::get("trail");
```

`get` is generic over `T: DeserializeOwned`. The stored JSON value
is deserialised on every read. Returns `None` when:

- The key isn't set
- No scope is active on the current task
- The stored value doesn't deserialise to `T` (e.g. you stored an
  `i64` and asked for a `String`)

The last case emits a `tracing::trace!` so the wrong-type bug is
observable — `Context::get` looking like "the value isn't set" when
it's really "the value is the wrong shape" is the kind of bug that
costs an hour to find without a log line pointing at it.

### `Context::hidden_get` — typed read from the hidden bag

Same shape as `get`, reads the hidden bag. Same wrong-type tracing
behaviour.

### `Context::has` — existence check on the visible bag

```rust
if Context::has("user_id") {
    // …
}
```

`has` only checks the visible bag (use `hidden_get(...).is_some()`
if you need to probe the hidden bag).

### `Context::all` — snapshot of the visible bag

```rust
let snapshot: HashMap<String, serde_json::Value> = Context::all();
```

Returns an empty `HashMap` outside a scope. This is what a JSON log
emitter should call to inject request-scoped fields into every log
line — and why the hidden bag exists separately.

### `Context::forget` — remove a key from both bags

```rust
Context::forget("trail");          // removes from visible AND hidden
```

The dual-bag removal is intentional. If you stored related data in
both bags (e.g. `user_id` visible, `user_email` hidden), one
`forget` cleans up both.

## Reading query parameters

`Context::query_param` reads from the URL's `?key=value` pairs
captured at request entry. The request middleware parses the query
string once into the scope's query bag, then every downstream caller
can read individual params by name without re-parsing:

```rust
use suprnova::Context;

let page: Option<String>   = Context::query_param("page");
let cursor: Option<String> = Context::query_param("cursor");
let sort: Option<String>   = Context::query_param("sort");
```

Returns `None` when the parameter is missing or no scope is active.
Duplicate keys follow Laravel's last-wins semantics — the same value
you'd get from the request's parsed query map.

### Pagination reads query params

This is why the query bag exists. Eloquent's paginators read `?page=`
and `?cursor=` straight off `Context::query_param`, so a handler
that returns a paginator doesn't need to plumb the page number
through manually:

```rust
use suprnova::Request;
use crate::models::Post;

pub async fn index(_req: Request) -> Response {
    // Reads ?page=N from the request's URL via Context::query_param
    // — no req.query() boilerplate, no parameter threading.
    let posts = Post::query()
        .order_by("created_at", "desc")
        .paginate(15)
        .await?;

    Ok(json_response!(posts))
}
```

Three paginator entry points use this:

- `Builder::paginate(per_page)` — reads `?page=`
- `Builder::simple_paginate(per_page)` — reads `?page=`
- `Builder::cursor_paginate(per_page)` — reads `?cursor=`

See [Pagination](pagination.md) for the full surface.

## Propagating into spawned tasks

`tokio::spawn` starts the child task with a fresh task-local
environment — the parent's `Context` scope does **not** flow in. A
bare `tokio::spawn` inside a request sees an empty `Context` and
every read returns `None`.

To carry the scope into a spawn, snapshot it with `Context::current()`
and re-enter it inside the child with `Context::scope`:

```rust
use suprnova::context::Context;

// Inside a request handler:
if let Some(store) = Context::current() {
    tokio::spawn(Context::scope(store, async move {
        // Now `Context::get`, `Context::query_param`, etc. see the
        // parent request's bag.
        let request_id: Option<String> = Context::get("_request_id");
        do_background_work(request_id).await;
    }));
}
```

The store returned by `Context::current()` shares the parent's
underlying maps via `Arc` — writes from the child are visible to the
parent for as long as the child holds the clone. This is exactly
what audit and logging spawns want: the child can stamp additional
keys (`Context::add("audit.completed", true)`) and the parent's
final log line sees them.

If you need an isolated snapshot (the child's writes shouldn't leak
back), build a fresh `ContextStore` and copy in just the keys you
need.

### Why bare `spawn` doesn't propagate

Tokio's task-locals (`tokio::task_local!`) are intentionally
task-scoped. Auto-inheriting across spawns would mean:

- Long-lived background tasks would pin parent context maps forever
- A panic in a child task could poison the parent's state
- The runtime would have to walk a parent pointer chain on every
  task-local read

The explicit `Context::current()` + `Context::scope` dance makes
propagation a deliberate decision instead of a hidden default.

## Tests

Inside `#[tokio::test]` or `#[suprnova_test]`, no `Context` scope is
installed by default. Most context-touching code under test handles
the "no scope" case gracefully (silent no-op + `None` reads), so
plain unit tests don't need any setup.

Two situations where the test needs help:

### When the code under test calls `query_param`

The pagination helpers read `?page=` via `Context::query_param`. A
unit test for "page 3 returns the right offset" needs `query_param`
to return `Some("3")`. Two ways:

**`test_query_guard` (recommended):**

```rust
use suprnova::Context;

#[tokio::test]
async fn paginate_reads_page_from_query() {
    let _q = Context::test_query_guard("page", "3");

    // Code under test now sees ?page=3
    assert_eq!(Context::query_param("page"), Some("3".into()));

    let posts = Post::query().paginate(15).await?;
    assert_eq!(posts.current_page(), 3);
}
// `_q` drops at end of scope — thread-local override is wiped.
```

`test_query_guard` returns an RAII guard. Even if the test body
panics, `Drop` runs and clears the thread-local override before the
OS thread is recycled. The guard is `#[must_use]` — binding it to
`_` clears immediately, which is almost never what you want.

**Bare `test_set_query` + `test_clear_query`:**

```rust
#[tokio::test]
async fn manual_pair() {
    Context::test_clear_query();        // wipe leak from any sibling
    Context::test_set_query("page", "5");

    // … assertions …

    Context::test_clear_query();
}
```

Use the guard form. The manual pair exists for cases where you need
multiple overrides set and cleared independently, but the
`#[must_use]` guard is harder to misuse.

Both APIs are gated by `#[cfg(any(test, feature = "testing"))]` —
they're compiled into test binaries and into release builds that
opt into the `testing` feature for integration test harnesses. They
do not exist in plain release builds.

### When the code under test reads or writes from a `Context` scope

Install one explicitly via `Context::scope`:

```rust
use suprnova::context::{Context, ContextStore};

#[tokio::test]
async fn handler_reads_tenant_id() {
    Context::scope(ContextStore::default(), async {
        Context::add("tenant_id", "acme");

        let resolved = my_helper_that_reads_tenant().await;
        assert_eq!(resolved, "acme");
    })
    .await;
}
```

Or seed a query bag at scope creation:

```rust
use std::collections::HashMap;
use suprnova::context::{Context, ContextStore};

#[tokio::test]
async fn handler_reads_query_from_scope() {
    let mut q = HashMap::new();
    q.insert("page".into(), "3".into());
    q.insert("sort".into(), "name".into());

    Context::scope(ContextStore::with_query(q), async {
        assert_eq!(Context::query_param("page"), Some("3".into()));
        assert_eq!(Context::query_param("sort"), Some("name".into()));
    })
    .await;
}
```

`ContextStore::with_query(HashMap)` is the same constructor the
request middleware uses, so a test exercising the same code path as
production sees the same shape of query bag.

### Why the thread-local override exists

The query-param override is a `thread_local!`, not a task-local.
That's deliberate: it lets tests install query params **without
wrapping every assertion in a `Context::scope` call**. The
combination is:

1. Reads check the thread-local override first
2. If no override, read the task-local `CONTEXT` scope's query bag
3. If no scope either, return `None`

The thread-local lookup costs effectively nothing in production
(the override is always empty outside test builds) and saves test
authors from boilerplate `Context::scope(...)` wrappers around every
paginate-related assertion.

## Common patterns

### Stamp the request id on every log

The framework already does this. The request middleware seeds
`_request_id` into the visible bag so downstream jobs, broadcasts,
and `Context::all()` log dumps can read the id by name. The same
middleware also opens a `tracing` span carrying the id as a span
field, which is what makes it show up on every log line emitted
inside the request — see [Logging](logging.md) for the subscriber
side. Reading the id from `Context` is the right path when you need
the value as a string (for example to plumb into an outbound HTTP
request as a correlation header):

```rust
let request_id: Option<String> = Context::get("_request_id");
```

### Carry tenant context into a queued job

`Context` doesn't auto-propagate across the queue serialise /
deserialise boundary — the worker runs in a different process from
the dispatcher, often on a different machine. Pass anything you
need into the job's payload:

```rust
use suprnova::{Context, FrameworkError, Queue};

// In a handler:
let tenant_id: String = Context::get("tenant_id")
    .ok_or_else(|| FrameworkError::param("tenant_id missing"))?;

Queue::push(SendInvoice { tenant_id, invoice_id }).await?;
```

When the worker processes `SendInvoice`, install a fresh `Context`
scope at the top of `Job::handle` and re-seed the keys you need from
the job payload — `Context::scope(ContextStore::default(), async {
... })` wrapping the body. Then any logging or deeply-nested helper
the job calls sees the same tenant id it would inside a request.

This is also where `hidden_add` earns its keep — the job can fetch
and stash an API key once at scope entry, and every downstream HTTP
call inside the job reads it via `Context::hidden_get` without
re-fetching. See [Queues](queues.md) for the `Job` trait shape.

### Audit trail across a request

```rust
Context::push("audit.steps", "validated_input");
// … more work …
Context::push("audit.steps", "charged_card");
// … more work …
Context::push("audit.steps", "sent_receipt");

// At response-time middleware:
let steps: Vec<String> = Context::get("audit.steps").unwrap_or_default();
tracing::info!(?steps, "request audit trail");
```

A response-time middleware that runs after the handler can dump the
audit trail in one log line, instead of every step's individual
debug line scattered across the request log.

### Hidden bag for SDK extension credentials

```rust
// At request entry, after auth:
Context::hidden_add("sdk.api_key", load_api_key_for(user_id));

// Deep inside an SDK call:
let key = Context::hidden_get::<String>("sdk.api_key")
    .ok_or_else(|| FrameworkError::param("api key not stashed"))?;
```

Logs that dump `Context::all()` don't show the key. The hidden bag
is the right place for any credential the handler needs to pass
deep into a call stack without exposing it to log surfaces.

## Why Suprnova diverges

Laravel's `Context` facade (introduced in Laravel 11) is the
inspiration — same method names, same visible/hidden split, same
"silent outside a request" contract. Two differences come from
Rust's runtime:

**Async propagation is explicit, not magical.** Laravel's `Context`
flows through queued jobs automatically because Laravel serialises
the context bag into the job payload at dispatch time. Rust's
async model doesn't have a single "current request" Thread-Locals
flow into — `tokio::spawn` starts fresh, and the queue boundary
involves serialisation across processes. Suprnova exposes the
propagation primitive (`Context::current()` + `Context::scope`) and
lets you opt into it at the boundary, instead of pretending tasks
inherit context they don't.

**Wrong-type reads are observable.** `get::<T>` on a value stored
as a different type silently returns `None` in Laravel (it's PHP,
the types weren't enforced at write time anyway). In Suprnova the
read emits a `tracing::trace!` because the wrong-type case
indicates a real bug — the value was written somewhere, just not
with the type you're reading. The trace lets you find it in
instrumented runs without changing the no-panic contract.

The third divergence is mechanical: Suprnova's `Context` is built on
`tokio::task_local!`, so its lifetime is bound to the Tokio task,
not to any global state. Cross-thread reads see the scope of the
**task currently running on that thread**, not whatever scope was
installed last. This is what makes the same `Context` facade safe
to call from a thread pool, an actor, or a `spawn_blocking` body —
provided you propagate the scope into the spawn.

## Where it lives

| Topic | File |
|---|---|
| `Context` facade + `ContextStore` | `framework/src/context/mod.rs` |
| Scope installation on HTTP request | `framework/src/logging/request_id.rs` |
| `Context::query_param` callers (pagination) | `framework/src/eloquent/builder.rs` |
| Re-exports | `framework/src/lib.rs` (`pub use context::{Context, ContextStore}`) |

## Next

- [Request Lifecycle](lifecycle.md) — where the `Context` scope is
  installed on every request
- [Service Container](container.md) — for cross-request state that
  outlives a single task
- [Logging](logging.md) — how `Context::all()` ends up in structured
  log lines
- [Pagination](pagination.md) — the main downstream reader of
  `Context::query_param`
- [Testing](testing.md) — `test_query_guard` and `Context::scope`
  patterns for unit tests
