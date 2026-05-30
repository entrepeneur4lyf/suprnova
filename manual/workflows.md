# Workflows

Workflows are durable, long-running async functions whose intermediate
state survives crashes, restarts, and panics. Reach for them when a unit
of work spans multiple steps — each potentially slow, fallible, or
side-effecting — and you cannot afford to lose progress halfway through.
A workflow's body runs once; each step's output is persisted; a retry
resumes from the first step that hasn't completed yet. Pair with
[`Queue`](queues.md) when the work is a one-shot job; pair with
[`Bus`](bus.md) when the work runs synchronously in the request task.

## Quick start

A workflow is an async function returning `Result<T, FrameworkError>`;
its body invokes one or more `#[workflow_step]` functions; you enqueue
it through the `start_workflow!` macro and a worker process drains it.

```rust
use suprnova::{workflow, workflow_step, start_workflow, FrameworkError};

#[workflow_step]
async fn fetch_user(user_id: i64) -> Result<String, FrameworkError> {
    Ok(format!("user:{}", user_id))
}

#[workflow_step]
async fn send_welcome_email(user: String) -> Result<(), FrameworkError> {
    // … actually send the mail
    Ok(())
}

#[workflow]
async fn welcome_flow(user_id: i64) -> Result<(), FrameworkError> {
    let user = fetch_user(user_id).await?;
    send_welcome_email(user).await?;
    Ok(())
}

// From a handler or any async context:
let handle = start_workflow!(welcome_flow, 123).await?;
```

The macro serialises the arguments to JSON, inserts a row in the
`workflows` table, and returns a [`WorkflowHandle`](#waiting-on-results)
identifying the enqueued instance. A separate worker process picks the
row up, runs the body, and persists each step's output as it goes.

`#[workflow]` collects the function into the workflow inventory under
its fully-qualified path (`module_path::fn_name`). Duplicate
registrations under the same name abort worker boot via
`registry::assert_no_duplicates` — silent shadowing would be
undebuggable, so the framework fails loud.

## Schema

Workflows persist into two tables: `workflows` (one row per instance)
and `workflow_steps` (one row per step invocation, keyed by
`(workflow_id, step_index)`). The framework owns the schema; you choose
when to apply it.

Two ways to wire the migrations.

### Generated migration files

The CLI scaffolds copies of the framework migrations into your app:

```bash
suprnova workflow:install
suprnova migrate
```

`workflow:install` writes `m_create_workflows_table.rs` and
`m_create_workflow_steps_table.rs` under `src/migrations/`, then
registers them in your `Migrator`. Use this when you want the schema
versioned alongside your other app migrations.

### Programmatic registration

Alternatively, register the framework-owned migration structs directly:

```rust
use sea_orm_migration::MigratorTrait;
use suprnova::workflow::migrations::{
    CreateWorkflowsTable, CreateWorkflowStepsTable,
};

pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![
            Box::new(CreateWorkflowsTable),
            Box::new(CreateWorkflowStepsTable),
        ]
    }
}
```

Both routes produce identical SQL. The same convention is used by
[`features::migrations`](feature-flags.md) and
[`payments::migrations`](payments.md).

## Running the worker

In a scaffolded app, the worker is started by the binary's
`workflow:work` subcommand:

```bash
suprnova workflow:work
```

The worker runs the same bootstrap your HTTP server does, so observers,
listeners, and container bindings registered in `bootstrap()` are
visible to workflow steps. On `SIGINT` / `SIGTERM` the worker stops
pulling new claims and awaits every in-flight workflow before exiting —
no workflow is orphaned mid-step on a clean shutdown.

The claim path (`claim_next_workflow`) uses
`FOR UPDATE SKIP LOCKED` against the `workflows` table, so the worker
process **requires Postgres**. SQLite and MySQL work for tests and for
the enqueue/persistence path, but the worker daemon will exit with an
error at first claim if the connection isn't Postgres.

## Configuration

Five environment variables tune the worker. Out-of-range values are
clamped to safe minimums with a `tracing::warn!` so a typo in `.env`
cannot brick the daemon.

| Variable | Default | Notes |
|---|---|---|
| `WORKFLOW_POLL_INTERVAL_MS` | `1000` | Sleep between empty claim rounds |
| `WORKFLOW_CONCURRENCY` | `4` | Max workflows running per worker (min 1) |
| `WORKFLOW_LOCK_TIMEOUT_SECS` | `30` | Lease duration before another worker may reclaim |
| `WORKFLOW_MAX_ATTEMPTS` | `3` | Per-workflow attempt budget (min 1) |
| `WORKFLOW_RETRY_BACKOFF_SECS` | `5` | Linear backoff: `attempts * value` (min 0) |

For programmatic configs (built in code rather than parsed from env),
call `WorkflowConfig::validate()` to fail fast on the same invariants
before constructing a `WorkflowWorker`.

## Crash recovery

Three layers of protection keep workflows from getting stuck on worker
failures.

**Panic boundary.** The workflow body runs inside
`AssertUnwindSafe(...).catch_unwind()`. A panic in any step is caught,
the payload is captured into the error column, and the row goes through
the same retry/fail accounting as a returned `Err`. Without the
boundary, a panic would skip the settlement path and leave the row at
`status='running'` forever.

**Lease heartbeat.** A long-running step that outlives
`WORKFLOW_LOCK_TIMEOUT_SECS` could otherwise have its lease expire under
its own feet. The worker spawns a heartbeat task that refreshes
`locked_until` at half the lock-timeout interval until the body
resolves. The heartbeat aborts on drop, so a returned `?` cannot leak a
renewal task and freeze the lease for a workflow nobody is running.

**Expired-lease reclaim.** When a worker dies without ever releasing
its lock (hard kill, host crash, kernel OOM), the row stays in
`status='running'` until `locked_until` passes. The claim query
explicitly picks up such rows: any `running` workflow whose lease has
expired becomes claimable by another worker on the next round, with
`attempts` incremented. Crash recovery is automatic — there's nothing
to script and no admin command to remember.

## Delivery semantics — at-least-once

Step bodies run with **at-least-once** semantics. A step may execute
more than once in two situations:

1. **Returned `Err`** — the workflow is requeued; on retry the failed
   step runs again, and any earlier steps replay from cache.
2. **Crash after the side effect, before `mark_step_succeeded` commits**
   — the lease expires, another worker reclaims, sees no cached output
   at that step index, and runs the body again.

The framework persists step **outputs** durably, but it cannot observe
the side effect itself. Step bodies are your responsibility to make
idempotent. Two patterns work for almost every case.

**Conditional writes.** Use `INSERT ... ON CONFLICT DO NOTHING`,
idempotency-key columns, or `seen_event_id` markers. Derive a stable
per-step key from data already in scope: the workflow's input
arguments plus a literal step tag (`("wf-charge", customer_id)`) is
enough because the same arguments map to the same workflow row across
retries.

**External idempotency keys.** Most third-party APIs (Stripe, SES, SQS)
accept an `Idempotency-Key` header. Pass a key derived from the
workflow's input plus a step-local tag (`format!("wf-charge-{}", customer_id)`)
so retried requests deduplicate at the provider.

Do **not** assume a step that returned `Ok` cannot run a second time —
a crash can land that second run on any subsequent worker, including
after a restart on a different host. See the
[Idempotency](idempotency.md) chapter for `Idempotency::once`,
`Idempotency::commit_on_success`, and `Idempotency::remember` —
all valid wrappers around a step body.

## Determinism contract

Workflows must be deterministic across replays. Each step is keyed by
`(step_name, step_index)`, and the framework caches its serialized
input alongside the output. When a step at the same index is replayed
with a different serialized input, the framework returns an error rather
than masking the corruption by returning the cached output from the
prior input.

In practice this means:

- Don't branch on `Utc::now()`, `rand::random()`, or other
  non-deterministic sources outside a `#[workflow_step]`. Step bodies
  can call them freely — their result is captured in the step output
  cache.
- Don't conditionally insert steps. If a retry hits a different number
  of steps before a given index, you get a step-name mismatch error.
  Put branching logic inside a step.
- Don't change step argument shapes between deploys without renaming
  the step. Renaming changes `step_name`, which restarts caching from
  scratch for that step.

## Waiting on results

`WorkflowHandle` lets the caller poll the row, wait for it to finish,
or fetch the serialised output.

```rust
use std::time::Duration;
use suprnova::{FrameworkError, WorkflowStatus};

let handle = start_workflow!(welcome_flow, 123).await?;

match handle.wait_with_timeout(Duration::from_secs(30)).await {
    Ok(WorkflowStatus::Succeeded) => { /* done */ }
    Ok(WorkflowStatus::Failed) => { /* persisted error column */ }
    Ok(_) => unreachable!("wait_* only returns terminal status"),
    Err(FrameworkError::Internal { message }) if message.contains("Timed out") => {
        // Workflow is still running; fall through to async UX.
    }
    Err(other) => return Err(other),
}
```

`wait()` polls indefinitely — use only in tests or short-lived scripts
where blocking forever is acceptable. For HTTP request paths,
`wait_with_timeout(Duration)` always wins against the inner poll loop,
even if the underlying status query stalls. A timeout error does **not**
cancel the workflow — the worker continues, and `handle.status().await`
returns the live state later.

`wait_with_options(Some(poll), Some(deadline))` exposes both knobs when
the defaults don't fit.

For typed outputs, define a `T: Serialize + DeserializeOwned` return on
the workflow and call `handle.output::<T>().await?`. The raw JSON is
available via `output_raw()`.

## Step caching, in detail

Step caching is keyed by **step name + step index**. The first
invocation of a step persists its input JSON, runs the body, and on
success persists the output JSON. A replay at the same index:

- Returns the cached output if the step is `succeeded` and the
  replayed input matches the cached input.
- Returns an error if the input differs (the determinism guard).
- Reruns the body if the step is `running` or `failed` (no cached
  output to return).

Step indexes are assigned by an `AtomicI32` per workflow context, so the
order is determined by the calls your workflow body makes. Branching
that produces a different step at the same index on a retry surfaces
as a step-name mismatch error rather than silently corrupting downstream
steps.

Outputs and inputs are stored as JSON TEXT, so all step return types
and arguments must be `Serialize + DeserializeOwned`.

## Detecting workflow context from a helper

`WorkflowContext::is_active()` returns whether the current task is
running under a workflow. Use it from helpers that need to behave
differently inside vs outside the worker — for example, a logger that
attaches the workflow tag only when one exists:

```rust
use suprnova::workflow::WorkflowContext;

fn maybe_workflow_tagged(message: &str) -> String {
    if WorkflowContext::is_active() {
        format!("[workflow] {message}")
    } else {
        message.to_string()
    }
}
```

Outside a workflow (called directly from a test or handler), a
`#[workflow_step]` function still runs — `WorkflowContext::current()`
simply returns `None`, the body executes without persistence, and the
step bypasses the cache entirely. That's intentional: it makes step
functions individually testable without standing up a worker.

### Why Suprnova diverges

Laravel doesn't have a first-class workflow primitive — jobs are
the closest neighbour, but they retry by re-running the whole job
body, not by resuming from the last successful step. Suprnova ships
workflows as a separate construct because Tokio makes the "stay
checked in to a slow async function for an hour" pattern cheap, and
because step-level persistence is the right abstraction for any
multi-step external interaction (provisioning a customer, running a
saga across two payment providers, generating a report that involves
several upstream APIs).

The design is closer to [DBOS](https://www.dbos.dev/) and
Cadence/Temporal than to a queue: durable state, deterministic replay,
explicit step boundaries. The difference from Temporal is operational
weight — there's no separate workflow service to run; the worker is
just `suprnova workflow:work` against your application database.

## Notes

- Step bodies can return any `Serialize + DeserializeOwned` type. The
  `()` unit type works for steps that exist only for their side effect.
- A `#[workflow_step]` function called outside a workflow context runs
  inline — no caching, no replay. This is how tests exercise step
  bodies directly.
- Step caching is `(step_name, step_index)`-keyed; rename a step (or
  reorder calls) and the caching resets for that step on the next
  replay.
- `start_workflow!` accepts any tuple of serializable arguments. Tuples
  preserve argument order so renaming positional parameters is safe;
  changing argument types is a schema break for any in-flight workflows.
- The framework's [observability](observability.md) layer captures
  worker structured logs (`worker_id`, `workflow_id`, `attempts`,
  `max_attempts`) on every settle path so you can audit retry budgets
  in production without instrumenting your steps.

## Next

- [Queues](queues.md) — one-shot background jobs with sync/redis/database drivers
- [Idempotency](idempotency.md) — wrappers for at-least-once delivery
- [Bus](bus.md) — synchronous command dispatch with typed results
- [Supervisors](supervisors.md) — long-lived task supervision with panic-catch auto-restart
- [Error Model](error-model.md) — `FrameworkError`, the panic boundary, and why settlement runs through `?`
