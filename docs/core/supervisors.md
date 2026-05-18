---
title: "Supervisors"
description: "Long-lived daemon tasks that restart automatically on failure, registered at boot via inventory and managed by SupervisorRegistry"
icon: "shield-check"
---

# Supervisors

A supervisor is a long-lived Tokio task that the framework starts at boot and restarts automatically when it exits. Supervisors are for "always-on" work: background heartbeats, metrics collectors, connection warmers, periodic sweepers, or any async loop that should never stop running. They are distinct from [queue workers](./queues.md), which consume discrete `Job` items from a queue. A supervisor has no job queue — it owns its own loop and decides when to sleep, wait, or act.

The `SupervisorRegistry` starts every registered supervisor as a detached Tokio task, watches each task's `JoinHandle`, and restarts it according to its `RestartPolicy` when it exits — whether by returning `Err`, returning `Ok`, or panicking. Restarts are separated by an exponential backoff that starts at 100ms and caps at 60 seconds, so a crashing supervisor does not spin-loop and flood logs.

## Quick Start

Define a supervisor, register it via `inventory::submit!`, and call `SupervisorRegistry::start_all()` at bootstrap.

**`src/supervisors/heartbeat.rs`:**

```rust
use async_trait::async_trait;
use std::time::Duration;
use suprnova::supervisor::{RestartPolicy, Supervisor};
use suprnova::FrameworkError;

pub struct LogHeartbeat;

#[async_trait]
impl Supervisor for LogHeartbeat {
    fn name(&self) -> &'static str { "heartbeat" }

    async fn run(&self) -> Result<(), FrameworkError> {
        loop {
            tracing::info!("supervisor heartbeat tick");
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }

    fn restart_policy(&self) -> RestartPolicy { RestartPolicy::Always }
}

inventory::submit!(suprnova::supervisor::SupervisorEntry {
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

**`Never` for one-shot work.** Prefer [queue workers](./queues.md) or [scheduled tasks](./scheduling.md) for work that runs on a schedule. Use `RestartPolicy::Never` when the supervisor pattern is convenient for something that must run once at startup and never again.

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

The backoff counter resets to zero on a successful run. A "successful run" for `OnError` is `Ok(())` returned; for `Always`, any exit is considered the basis for the next backoff decision (consecutive exits accumulate backoff; the counter resets after a run that was alive long enough to be considered healthy — the exact threshold is internal to the registry).

The 60-second cap prevents a permanently-broken supervisor from sleeping indefinitely or hammering external dependencies on every retry. Combine with `error!`-level logging to alert when a supervisor enters the high-backoff band.

## Observability

Every restart emits an `error!`-level log entry with:

- The supervisor name (from `Supervisor::name()`).
- The backoff delay in milliseconds before the next spawn.
- The error message from `run()`'s `Err` return value, or `"panic"` for a caught panic.

```
ERROR suprnova::supervisor: supervisor "heartbeat" exited with error: connection refused; restarting in 400ms
ERROR suprnova::supervisor: supervisor "heartbeat" panicked; restarting in 800ms
```

Supervisors do not get an automatic tracing span around `run()` — the registry spans the lifecycle (start, restart) but not the interior of the task. Emit your own `info_span!` or `instrument` your loop body if you want span context on work done inside the supervisor:

```rust
async fn run(&self) -> Result<(), FrameworkError> {
    loop {
        let span = tracing::info_span!("heartbeat.tick");
        let _guard = span.enter();
        do_work().await?;
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}
```

## Out of v1 Scope

The following items are intentionally deferred:

- **Graceful shutdown draining.** In v1, supervisor tasks are detached — they are not joined on process shutdown. Work in progress when the process receives a shutdown signal is abandoned. Drain-on-shutdown (waiting for current loop iterations to complete before exiting) is a follow-on phase concern.

- **Supervisor trees (parent/child).** There is no hierarchy — all supervisors are peers under the single `SupervisorRegistry`. Structured supervision (where one supervisor owns and restarts child supervisors) is orchestrator territory.

- **Resource limits (cgroup, memory, CPU).** Apply resource constraints through systemd unit files (`MemoryMax=`, `CPUQuota=`) or Kubernetes resource requests/limits at the pod level. The framework does not impose process-internal resource limits on individual supervisor tasks.

- **Multi-machine supervision.** Supervisors run within a single process on a single machine. Distributing supervision decisions across machines is orchestrator territory (Kubernetes, Nomad, systemd on multiple hosts).

## Reference

| Symbol | Purpose |
|--------|---------|
| `suprnova::supervisor::Supervisor` | Trait to implement on your supervisor struct. Required methods: `name() -> &'static str`, `async fn run(&self) -> Result<(), FrameworkError>`. Optional: `restart_policy() -> RestartPolicy` (defaults to `OnError`). |
| `suprnova::supervisor::RestartPolicy` | Enum with variants `OnError`, `Always`, `Never`. Controls when the registry spawns a replacement task. |
| `suprnova::supervisor::SupervisorEntry` | Inventory item. Declare `factory: fn() -> Box<dyn Supervisor>`. Submit one entry per supervisor via `inventory::submit!(SupervisorEntry { factory: || Box::new(MySupervisor) })`. |
| `suprnova::supervisor::SupervisorRegistry::start_all()` | Async fn. Iterates all submitted `SupervisorEntry` values, spawns each supervisor as a detached Tokio task, and begins monitoring for restarts. Call once from your bootstrap `register()`. |
