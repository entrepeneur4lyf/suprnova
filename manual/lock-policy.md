# Lock Policy

Suprnova is a single long-lived Tokio process, not a fleet of short-lived
PHP workers. Every process-global registry, singleton, and shared cache
you bind at boot outlives every request that touches it. That changes one
small but consequential thing about how you reach for `std::sync::Mutex`
and `std::sync::RwLock`: a panic while holding a guard *poisons the lock*
for the rest of the process's life, and the next caller has to decide
what to do about it. This chapter is the project-wide policy for that
decision — two sanctioned patterns, when to pick which, and why you
should never reach for a raw `.lock().unwrap()` in framework or
application code.

## Why this chapter exists

In Laravel you never thought about poisoned locks because there were
none. PHP is shared-nothing: a fatal error tears down one request's
process, the next request starts in a fresh one, no in-memory state
survives to corrupt. Suprnova runs the opposite way. The process boots
once, registries get populated, and they stay alive for the entire
lifetime of the binary. A handler that panics while holding a write
guard on a process-global `RwLock` leaves that lock *poisoned* — every
subsequent `.read()` and `.write()` returns `Err(PoisonError)` forever,
unless someone explicitly recovers it.

The default Rust idiom — `.lock().unwrap()` — converts that `Err` into a
panic. Which then becomes another poisoned lock somewhere up the stack.
Which then takes down the next subsystem that touches it. One bad request
cascades into a half-dead process.

The policy below prevents that cascade.

> **Scope.** This applies to `std::sync::Mutex` and `std::sync::RwLock`,
> which carry poison state. The async cousins in `tokio::sync` (`Mutex`,
> `RwLock`, `Semaphore`) do *not* poison — a panic while holding a
> `tokio::sync::Mutex` guard drops the guard cleanly and the next
> `.lock().await` succeeds. If your hot path is async and you don't
> need to acquire the guard from a sync context (a `Drop` impl, a
> framework callback, a CLI subcommand), prefer the Tokio variants and
> the question goes away.

## The two sanctioned patterns

Every place in the framework that holds a `std::sync` lock uses one of
exactly two patterns. Pick the same way in your own code.

### Pattern 1 — Map poison to a returned error

When the caller already returns `Result<_, E>` and one more `?` doesn't
change its shape, surface the poison as an error and let the request
fail cleanly. The framework uses internal `pub(crate)` helpers
(`lock::read`, `lock::write`, `lock::lock`) that map a poisoned guard
to `FrameworkError::internal("<context> lock poisoned")`, embedding a
caller-supplied label so logs can tell which subsystem poisoned without
every call site wrapping the error itself.

The pattern those helpers encode is short enough to write inline in your
application code:

```rust
use std::collections::HashMap;
use std::sync::RwLock;
use suprnova::FrameworkError;

static FEATURE_FLAGS: RwLock<HashMap<String, bool>> = RwLock::new(HashMap::new());

pub fn enable(flag: &str) -> Result<(), FrameworkError> {
    let mut guard = FEATURE_FLAGS
        .write()
        .map_err(|_| FrameworkError::internal("feature flags lock poisoned"))?;
    guard.insert(flag.to_string(), true);
    Ok(())
}

pub fn is_enabled(flag: &str) -> Result<bool, FrameworkError> {
    let guard = FEATURE_FLAGS
        .read()
        .map_err(|_| FrameworkError::internal("feature flags lock poisoned"))?;
    Ok(guard.get(flag).copied().unwrap_or(false))
}
```

Inside a handler, `is_enabled(...)?` collapses through the same
`FrameworkError → HttpResponse` path every other framework error uses:
the client gets a sanitised 500 with `{"message": "Internal Server
Error"}`, the structured log captures the labelled poison message, the
request id is preserved end-to-end, and the rest of the process keeps
serving traffic. See the [Errors](errors.md) chapter for the full
conversion path.

Use this pattern when:

- The caller already returns `Result` (most fallible operations do).
- A poisoned lock represents a real, unrecoverable failure of the
  subsystem — there is no sane "partial truth" to fall back to.
- You want operators to *see* the poison in logs the next time the
  subsystem is touched. The labelled message is your forensic crumb.

The framework's notifications dispatcher, mail transport, mailable
registry, db event listeners, and named connection registry all use this
pattern. A panic in any one of them surfaces as a 500 on the next
request that hits the registry; everything else keeps running.

### Pattern 2 — Recover in place with `into_inner()`

When the caller's signature is *not* fallible (a `bool` lookup, a hot
routing check, a path the request lifecycle relies on) or when the
shared state is structurally safe to use after a partial write,
recover the guard and continue:

```rust
use std::collections::HashMap;
use std::sync::RwLock;

static ALLOWED_INCLUDES: RwLock<HashMap<&'static str, Vec<&'static str>>> =
    RwLock::new(HashMap::new());

pub fn allows(dto: &str, field: &str) -> bool {
    ALLOWED_INCLUDES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(dto)
        .map(|fields| fields.contains(&field))
        .unwrap_or(false)
}

pub fn register(dto: &'static str, fields: &'static [&'static str]) {
    let mut guard = ALLOWED_INCLUDES
        .write()
        .unwrap_or_else(|e| e.into_inner());
    guard.insert(dto, fields.to_vec());
}
```

`PoisonError::into_inner()` returns the guard despite the poison.
Subsequent reads and writes proceed normally — the lock stays poisoned
for `is_poisoned()` queries, but data flow is restored.

The framework uses this pattern in `data::registry` (the include-set
allowlist read on every JSON:API response), `auth::manager` (the named
auth-provider map), `app::paths` (the resolved-paths cache), the
testing fakes for mail and events, and the loaded-env-keys map in
config. Every one is a place where either no caller has a `Result` to
return, or the state is append-only and structurally safe to keep using.

Use this pattern when:

- The caller's signature is plain (`bool`, `&str`, a clone of a stored
  value) and changing it to `Result` would force every caller — sometimes
  every framework subsystem — to bubble.
- The shared state can tolerate a partial write. Append-only maps and
  caches are the typical shape: the worst case is a missing or stale
  entry, which the caller already handles (default-deny, fall back to
  primary, recompute).
- The hot path runs often enough that returning an error on every
  subsequent request would be operationally worse than degrading.

## How to choose between them

The decision rule, in one sentence: **if the worst case of using
post-poison state is a wrong answer with consequences, map to an error;
if it's a missing or stale entry the caller already handles, recover
in place.**

Walk it through:

1. **Is the caller's signature `Result<_, E>`?** If no, you have to
   recover in place — adding `Result` to a `bool` is usually a
   project-wide refactor and not worth it for a poison edge.
2. **If a half-written value were observed, would the application
   make a wrong decision with real-world consequences?** Charging a
   wrong customer, allowing an unauthorised include, granting access
   to the wrong tenant — that's "yes, map to an error." Returning
   `false` to "is this name registered?" and falling back to the
   primary pool — that's "no, recover in place."
3. **Is the state append-only or naturally idempotent on re-registration?**
   If yes, recover-in-place is safe. If a write is a state-machine
   transition that depends on the prior value, prefer map-to-error so
   you don't compound a corruption.

When in doubt, map to an error. A request returning 500 is a loud
signal you can fix; silent wrong answers are not.

## Never reach for `.lock().unwrap()`

The forbidden shape:

```rust
// NEVER — one panic anywhere in the call graph below
// this line poisons the lock and every subsequent caller
// turns the poison into another panic.
let mut guard = SOMETHING.lock().unwrap();
```

`.expect("…")` is the same thing with a nicer message. Both convert a
poisoned-lock `Err` into a panic that the request-lifecycle's
`AssertUnwindSafe(...).catch_unwind()` net catches and converts to a
500 — that net is a *last line of defence*, not licence to skip the
decision above. Public framework APIs and application code must pick
one of the two sanctioned patterns.

The two exceptions where `.unwrap()` is acceptable on a `std::sync`
lock:

- **Test setup that *wants* to assert poisoning was reached** —
  `framework/src/lock.rs`'s own poison-induction helper uses
  `.unwrap()` inside the panicking thread on purpose.
- **The error path of a poisoning operation that already failed** — by
  the time you're inside `poison_rw(...)`'s thread, the panic *is* the
  point.

If you're not in one of those, pick a pattern from the section above.

## What if my function returns `bool`?

This is the situation `ConnectionRegistry::has` lives in. It's a
`bool` lookup on the hot path of the executor's read-replica routing,
called inline as `if ConnectionRegistry::has("read_replica").await { … }`.
Widening it to `Result<bool, FrameworkError>` would force every caller
in the executor to `?`-bubble, propagating an internal-error code path
into routing decisions that just want a yes/no.

The recover-in-place pattern handles this — return `false` and let the
caller's fallback logic kick in (here, the executor drops back to the
primary pool, which is the safe behaviour anyway). To make sure
operators still see the condition, emit a one-shot `tracing::warn!` the
first time poison is observed:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;
use std::collections::HashMap;

static REGISTRY: RwLock<HashMap<String, ()>> = RwLock::new(HashMap::new());
static POISON_WARNED: AtomicBool = AtomicBool::new(false);

pub fn has(name: &str) -> bool {
    match REGISTRY.read() {
        Ok(g) => g.contains_key(name),
        Err(_) => {
            // Race-safe: only the first observer logs.
            if !POISON_WARNED.swap(true, Ordering::SeqCst) {
                tracing::warn!(
                    target: "myapp::registry",
                    "registry lock poisoned — `has({name})` degrading to false",
                );
            }
            false
        }
    }
}
```

The `swap`-based gate matters: `RwLock` poison is sticky, so without
the gate every subsequent call would re-fire the warning and flood your
logs. With the gate, you get exactly one warn per process per registry,
and a corresponding `Result`-returning getter (`get`, `register`) on
the same registry will surface the poison the next time anything
*actually needs* the lookup to succeed. That gives operators both
signals: an early "something is wrong" warn, and a hard 500 the moment
a request truly depended on the registry.

## What the framework already protects

You don't have to apply this policy to any state the framework owns —
it's already in place. Concretely:

- The named connection registry (`ConnectionRegistry::register`, `get`,
  `has`) maps poison to `FrameworkError::internal` on the writes and
  `Result`-returning reads; `has` degrades to `false` with the
  warn-once gate.
- The notifications dispatcher and factory registry, mailable registry,
  mail transport, mail memory capture, and DB event listeners all
  return `FrameworkError::internal` on poison.
- The `data::registry` include allowlist, `auth::manager` provider
  map, `app::paths`, the loaded-env-keys cache, and the in-memory
  testing fakes all recover in place.

Where you intersect those subsystems through their public API
(`Notification::send`, `Mail::send`, `Auth::user`, `DB::connection`,
the JSON:API response path), a poisoned framework lock surfaces as a
clean 500 — never a panic at your call site.

## Why Suprnova diverges

Laravel doesn't have a lock policy because it doesn't have long-lived
shared state. Each PHP request gets its own process, its own memory,
its own copies of every singleton. There's no in-memory registry to
poison and no concept of "the next request" inheriting damage from
the previous one — the runtime guarantees a clean slate.

Suprnova is built on Tokio, which gives you exactly the long-lived
shared state that PHP rules out. Cheap WebSockets, in-memory caches,
connection pools you don't pay to rebuild — all of these need
process-global registries that outlive any single request. That
capability is the whole point of moving to Rust for this style of app
(see the [introduction](introduction.md) for the framework's full
motivation). The cost of having it is that you now have to think
about what happens when a panicked thread leaves shared state in a
guarded condition, because there *is* shared state to leave.

The two-pattern policy is the smallest answer that keeps the
capability and removes the cost. Recover in place where the state is
safe to keep using; map to an error where you'd rather have a clean
500 than a wrong answer. Both options leave the rest of the process
serving traffic. Neither leaves a panicked unwrap waiting to take
down the subsystem above.

This is the same shape as the [fail-open vs fail-closed
decision](rate-limiting.md) the framework applies to unreachable cache
and rate-limit backends: an explicit policy choice at the call site,
not a default. Async-everywhere gives you long-lived state; the
framework gives you the playbook for keeping it honest.

## Next

- [Errors](errors.md) — how `FrameworkError::internal` becomes the
  sanitised 500 the client receives, with the labelled poison message
  preserved in your structured log.
- [Container](container.md) — where the process-global registries this
  policy protects actually live, and why task-local/thread-local
  scoping keeps tests from inheriting each other's bindings.
- [Lifecycle](lifecycle.md) — the panic boundary
  (`execute_chain_safely`) that catches the *last-resort* unwrap and
  converts it to a 500, so you understand exactly what the safety net
  does and why it isn't an excuse to skip the policy above.
- [Rate Limiting](rate-limiting.md) — the parallel `BackendErrorPolicy`
  story for backends that can be *unreachable* rather than poisoned;
  same explicit-choice principle, different failure mode.
- [Testing](testing.md) — how `TestContainer::fake` and the
  thread-local container layer keep parallel tests from polluting each
  other's registries, which is the test-time complement to the
  poison-handling story.
