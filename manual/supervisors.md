# Supervisors

A supervisor is a long-lived Tokio task that the framework starts at boot and restarts automatically when it exits. Supervisors are for "always-on" work: background heartbeats, metrics collectors, connection warmers, periodic sweepers, or any async loop that should never stop running. They are distinct from [queue workers](queues.md), which consume discrete `Job` items from a queue. A supervisor has no job queue — it owns its own loop and decides when to sleep, wait, or act.

The `SupervisorRegistry` starts every registered supervisor as a detached Tokio task, watches each task's `JoinHandle`, and restarts it according to its `RestartPolicy` when it exits — whether by returning `Err`, returning `Ok`, or panicking. Restarts are separated by an exponential backoff that starts at 100ms and caps at 60 seconds, so a crashing supervisor does not spin-loop and flood logs.

## Quick Start

Define a supervisor, register it via `inventory::submit!`, and call `SupervisorRegistry::start_all()` at bootstrap.

**`src/supervisors/heartbeat.rs`:**

```rust
use async_trait::async_trait;
use std::time::Duration;
use suprnova::supervisor::{RestartPolicy, Supervisor};
use suprnova::{FrameworkError, SupervisorEntry};
use tokio_util::sync::CancellationToken;

pub struct LogHeartbeat;

#[async_trait]
impl Supervisor for LogHeartbeat {
    fn name(&self) -> &'static str { "heartbeat" }

    async fn run(&self, cancel: CancellationToken) -> Result<(), FrameworkError> {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    tracing::info!("supervisor heartbeat tick");
                }
            }
        }
    }

    fn restart_policy(&self) -> RestartPolicy { RestartPolicy::Always }
}

// Use the re-exported `suprnova::inventory` so a scaffolded app doesn't need
// to add `inventory` as a direct dependency.
suprnova::inventory::submit!(SupervisorEntry {
    factory: || Box::new(LogHeartbeat),
});
```

**`src/bootstrap.rs`:**

```rust
use suprnova::supervisor::SupervisorRegistry;

pub async fn register() {
    SupervisorRegistry::start_all().await;
}
```

That is the full setup. The `LogHeartbeat` supervisor starts at boot, logs every 60 seconds, and — because `RestartPolicy::Always` restarts on both `Ok` and `Err` exits — is restarted immediately if the loop ever exits for any reason.

## Restart Policies

Each supervisor declares its `RestartPolicy` via the trait method. The default is `OnError`.

| Policy | Restarts when... | Use case |
|--------|-----------------|----------|
| `RestartPolicy::OnError` | `run()` returns `Err` or panics | Tasks that should run to completion on success (e.g., a one-time init job wrapped as a supervisor). |
| `RestartPolicy::Always` | `run()` returns either `Ok` or `Err`, or panics | True daemons — loops that should never return. If the loop exits for any reason, that is a bug and a restart is warranted. |
| `RestartPolicy::Never` | (never) | One-shot tasks that should run once and not be restarted regardless of outcome. |

```rust
fn restart_policy(&self) -> RestartPolicy { RestartPolicy::OnError }   // default
fn restart_policy(&self) -> RestartPolicy { RestartPolicy::Always }    // daemon loop
fn restart_policy(&self) -> RestartPolicy { RestartPolicy::Never }     // one-shot
```

**When to pick `Always` vs `OnError`.** An infinite loop supervisor (`loop { ... }`) should use `Always` — if the loop ever returns `Ok(())`, something unexpected happened and a restart is the correct response. A supervisor that does finite work and returns `Ok` on success (e.g., refreshing a cache once) should use `OnError` so that a clean finish does not trigger a restart.

**`Never` for one-shot work.** Prefer [queue workers](queues.md) or [scheduled tasks](scheduling.md) for work that runs on a schedule. Use `RestartPolicy::Never` when the supervisor pattern is convenient for something that must run once at startup and never again.

## Panic Handling

Panics inside `run()` are caught by the registry and treated as errors — a panicking supervisor is restarted with backoff rather than crashing the process. The registry monitors each supervisor's `JoinHandle` and detects panics via the standard Tokio join mechanism.

From the restart-policy perspective, a panic is always treated as an `Err` exit regardless of the policy:

- `OnError` — restarts after a panic (panic counts as error).
- `Always` — restarts after a panic (same as any other exit).
- `Never` — does not restart after a panic (same as any other exit).

The panic is logged at `error!` level with the supervisor name before the restart backoff begins.

## Backoff

When a supervisor exits and its policy says to restart, the registry waits before spawning the replacement:

| Restart | Delay |
|---------|-------|
| 1st | 100ms |
| 2nd | 200ms |
| 3rd | 400ms |
| 4th | 800ms |
| ... | doubles each time |
| Capped | 60s |

The backoff does **not** reset. The counter is initialised once when the restart loop starts and only ever doubles, capped at 60 s, for the entire lifetime of the supervisor task. A long-lived supervisor that flaps occasionally will eventually sleep 60 s between retries until the process restarts. There is no liveness-based reset and no "healthy enough" threshold — accumulating backoff is the design.

The 60-second cap prevents a permanently-broken supervisor from sleeping indefinitely or hammering external dependencies on every retry. Combine with `error!`-level logging to alert when a supervisor enters the high-backoff band.

## Graceful Shutdown

Supervisors receive a `CancellationToken` as a parameter to `run()`. The framework cancels this token on Ctrl-C / SIGTERM as part of `Server::run`'s shutdown sequence. Supervisors that want to flush state, finish in-flight work, or otherwise exit cleanly should `tokio::select!` on `cancel.cancelled()`:

```rust
async fn run(&self, cancel: CancellationToken) -> Result<(), FrameworkError> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(60)) => {
                tracing::info!("supervisor heartbeat tick");
            }
        }
    }
}
```

The framework drains the supervisor JoinSet with a 5-second grace window after cancellation. Supervisors that do not honor the token within that window get aborted via `JoinSet::abort_all`. The drain runs after the WebSocket handler drain (so WS connections clean up first) and before telemetry buffers flush.

Supervisors that ignore the token entirely will run until the 5-second window expires and then be forcibly aborted. If your supervisor holds resources that need flushing (open file handles, in-flight HTTP requests, partially-written records), always select on `cancel.cancelled()` and clean up before returning.

### Embedders and integration tests

`Server::run` calls `SupervisorRegistry::shutdown(...)` for you. Code that calls `SupervisorRegistry::start_all()` outside of `Server::run` (embedders driving the framework from a custom binary, or integration tests that spin up supervisors directly) must also call `SupervisorRegistry::shutdown(timeout)` at teardown, or supervisor tasks will leak past the lifetime of the test:

```rust
use std::time::Duration;
use suprnova::SupervisorRegistry;

// Test setup
SupervisorRegistry::start_all().await;

// ... exercise the supervisor ...

// Test teardown — cancels the shared token, drains the JoinSet up
// to `timeout`, then `abort_all` for stragglers.
SupervisorRegistry::shutdown(Duration::from_secs(1)).await;
```

`shutdown` is a no-op if `start_all` was never called, so it is safe to call from teardown unconditionally.

## Observability

Every error-path restart emits an `error!`-level log entry with structured fields:

- `supervisor` — from `Supervisor::name()`.
- `error` — the error message from `run()`'s `Err` return value, or `"panic: <payload>"` for a caught panic, or `"join error: <detail>"` for an unusual join failure.
- `backoff_ms` — the backoff delay in milliseconds before the next spawn.

Panics are reported through the same error log — there is no separate "panicked" message:

```
ERROR suprnova::supervisor: supervisor errored; restarting after backoff supervisor=heartbeat error=connection refused backoff_ms=400
ERROR suprnova::supervisor: supervisor errored; restarting after backoff supervisor=heartbeat error="panic: \"deliberate test panic\"" backoff_ms=800
```

`RestartPolicy::Always` returning `Ok(())` emits a `warn!` (not `error!`) with the same `supervisor` / `backoff_ms` fields and the message "supervisor returned Ok under Always policy; restarting" — useful for spotting daemon loops that exited cleanly when they shouldn't have.

Supervisors do not get an automatic tracing span around `run()` — the registry spans the lifecycle (start, restart) but not the interior of the task. Emit your own `info_span!` or `instrument` your loop body if you want span context on work done inside the supervisor:

```rust
async fn run(&self, cancel: CancellationToken) -> Result<(), FrameworkError> {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = async {
                let span = tracing::info_span!("heartbeat.tick");
                let _guard = span.enter();
                do_work().await.ok();
                tokio::time::sleep(Duration::from_secs(60)).await;
            } => {}
        }
    }
}
```

### Why Suprnova diverges

Laravel has no direct equivalent. PHP's request-per-process model makes always-on in-process daemons impossible — long-lived work has to live outside the request lifecycle, typically as a `supervisord`-managed worker process consuming a queue or a cron-scheduled command. Laravel's queue worker (`php artisan queue:work`) is the closest analogue, but it is still a one-shot CLI process that an external supervisor restarts.

Suprnova runs on Tokio inside a single long-lived process. Always-on background tasks fit naturally as supervised Tokio tasks alongside the HTTP server — no extra process boundary, no external supervisor, no separate IPC channel for state. The `Supervisor` trait is the in-process equivalent of `supervisord`, scoped to the framework's own task tree, with the same restart-on-exit + backoff guarantees.

`Queue` workers (which Laravel has) still ship — see [Queues](queues.md) — for discrete-job work. Supervisors cover the "always tick" case that Laravel pushes out of the framework boundary entirely.

## Out of v1 Scope

The following items are intentionally deferred:

- **Supervisor trees (parent/child).** There is no hierarchy — all supervisors are peers under the single `SupervisorRegistry`. Structured supervision (where one supervisor owns and restarts child supervisors) is orchestrator territory.

- **Resource limits (cgroup, memory, CPU).** Apply resource constraints through systemd unit files (`MemoryMax=`, `CPUQuota=`) or Kubernetes resource requests/limits at the pod level. The framework does not impose process-internal resource limits on individual supervisor tasks.

- **Multi-machine supervision.** Supervisors run within a single process on a single machine. Distributing supervision decisions across machines is orchestrator territory (Kubernetes, Nomad, systemd on multiple hosts).

## Reference

The four primary types — `Supervisor`, `RestartPolicy`, `SupervisorEntry`, `SupervisorRegistry` — are re-exported at the crate root (`suprnova::Supervisor`, etc.) in addition to the longer `suprnova::supervisor::*` path. The two free accessors stay under `suprnova::supervisor::*`.

| Symbol | Purpose |
|--------|---------|
| `Supervisor` | Trait to implement on your supervisor struct. Required methods: `name() -> &'static str`, `async fn run(&self, cancel: CancellationToken) -> Result<(), FrameworkError>`. Optional: `restart_policy() -> RestartPolicy` (defaults to `OnError`). The `cancel` token is signalled on process shutdown; select on `cancel.cancelled()` to exit cleanly before the 5-second abort window expires. |
| `RestartPolicy` | Enum with variants `OnError`, `Always`, `Never`. Controls when the registry spawns a replacement task. |
| `SupervisorEntry` | Inventory item. Declare `factory: fn() -> Box<dyn Supervisor>`. Submit one entry per supervisor via `suprnova::inventory::submit!(SupervisorEntry { factory: || Box::new(MySupervisor) })`. |
| `SupervisorRegistry::start_all()` | Async fn. Iterates all submitted `SupervisorEntry` values, spawns each supervisor as a detached Tokio task into the per-process JoinSet, and begins monitoring for restarts. Idempotent — the per-process statics are `OnceLock`s. Call once from your bootstrap `register()`. |
| `SupervisorRegistry::shutdown(timeout)` | Async fn. Cancels the shared cancellation token so every supervisor watching `cancel.cancelled()` exits, drains the JoinSet up to `timeout`, then `abort_all` for stragglers. `Server::run` invokes this as part of its shutdown sequence; embedders and integration tests that call `start_all` outside `Server::run` must call this themselves to avoid leaking tasks. No-op if `start_all` was never called. |
| `suprnova::supervisor::supervisor_tasks()` / `supervisor_cancel_token()` | Accessors that return `Option<&'static …>` to the underlying JoinSet and cancellation token. Used by `Server::run`'s shutdown sequence; exposed `pub` so embedders driving the framework from a custom binary can integrate. Application code should not need these. |

## Next

- [Queues](queues.md) — supervisor-vs-queue-worker decision and the discrete-job alternative
- [Scheduling](scheduling.md) — for periodic work that doesn't need a long-lived loop
- [Workflows](workflows.md) — for stateful, long-running work that needs durable resume
- [Broadcasting](broadcasting.md) — uses the same shutdown sequence (drain ordering)
- [Request Lifecycle](lifecycle.md) — where `Server::run` and the shutdown drain fit in
