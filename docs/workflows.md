# Workflows

suprnova includes a durable, Postgres‑backed workflow engine with step persistence and automatic retries. Workflows resume from the last successful step on retry.

## Install Migrations

Run the install command once per app, then migrate:

```bash
suprnova workflow:install
suprnova migrate
```

This creates two tables:

- `workflows`
- `workflow_steps`

## Define Steps and Workflows

Use attribute macros to define steps and workflows. Steps are cached automatically when invoked inside a workflow.

```rust
use suprnova::{workflow, workflow_step, start_workflow, FrameworkError};

#[workflow_step]
async fn fetch_user(user_id: i64) -> Result<String, FrameworkError> {
    // Any I/O or queries here
    Ok(format!("user:{}", user_id))
}

#[workflow_step]
async fn send_welcome_email(user: String) -> Result<(), FrameworkError> {
    println!("Sending email to {}", user);
    Ok(())
}

#[workflow]
async fn welcome_flow(user_id: i64) -> Result<(), FrameworkError> {
    let user = fetch_user(user_id).await?;
    send_welcome_email(user).await?;
    Ok(())
}
```

## Enqueue a Workflow

Use `start_workflow!` to enqueue. Arguments are serialized as JSON and stored in Postgres.

```rust
let handle = start_workflow!(welcome_flow, 123).await?;
let status = handle.wait().await?;
```

## Run the Worker

Run workers as a separate process in production (similar to scheduled jobs):

```bash
suprnova workflow:work
```

## Configuration

Set these environment variables as needed:

- `WORKFLOW_POLL_INTERVAL_MS` (default: `1000`)
- `WORKFLOW_CONCURRENCY` (default: `4`)
- `WORKFLOW_LOCK_TIMEOUT_SECS` (default: `30`)
- `WORKFLOW_MAX_ATTEMPTS` (default: `3`)
- `WORKFLOW_RETRY_BACKOFF_SECS` (default: `5`)

Retry backoff is linear: `attempt * WORKFLOW_RETRY_BACKOFF_SECS`.

## Delivery Semantics — At‑Least‑Once

Workflow steps execute with **at‑least‑once** semantics. A step body may
run more than once in two situations:

1. **Returned `Err`** — the workflow is requeued; on retry the failed
   step runs again (succeeded earlier steps replay from cache).
2. **Crash after the side effect, before the step succeeds** — if the
   worker process dies, panics, or has its lease expire after the step
   has performed its external effect but before
   `mark_step_succeeded` commits, the step row stays at `running`. The
   next claim sees no cached output and re-executes the body.

The framework records and replays step **outputs** durably, but it
cannot observe the side effect itself. That makes step bodies the
caller's responsibility to make idempotent. Two patterns:

- **Conditional writes.** Use `INSERT ... ON CONFLICT DO NOTHING`, idempotency keys, or a `seen_event_id` column. The workflow id and step index are both available on `WorkflowContext::current()` and make stable idempotency keys: `format!("wf:{}:step:{}", wf_id, step_index)`.
- **External idempotency keys.** Most third‑party APIs (Stripe, SES, etc.) accept an `Idempotency-Key` header. Pass `format!("wf-{}-{}-{}", wf_id, step_index, action)` so retried requests deduplicate at the provider.

Do **not** assume a step that returned `Ok` cannot run a second time —
a crash can land that second run on any subsequent worker, including
after a restart on a different host.

> **Pure side‑effect helpers exist.** If a side effect is genuinely
> impossible to make idempotent (e.g. "send this exact message twice
> if needed, the user can tolerate duplicates"), document the choice in
> a comment on the step function so future readers know it was
> deliberate.

## Configuring `WorkflowHandle::wait`

`handle.wait()` polls until the workflow finishes — useful in tests and
short‑lived scripts, but it can hang forever if the workflow is stuck.
Use `wait_with_timeout(Duration)` for HTTP request paths so a lost
worker cannot strand a request:

```rust
use std::time::Duration;
use suprnova::FrameworkError;

let handle = start_workflow!(welcome_flow, 123).await?;
match handle.wait_with_timeout(Duration::from_secs(30)).await {
    Ok(status) => {
        // Workflow finished within 30 s.
    }
    Err(FrameworkError::Internal(_)) => {
        // Timeout — fall through to async / polling UX. The workflow
        // is still running; call `handle.status().await` later.
    }
    Err(other) => return Err(other),
}
```

For full control over polling cadence and timeout together, use
`wait_with_options(Some(poll), Some(deadline))`.

## Notes

- The worker requires **Postgres** (uses `FOR UPDATE SKIP LOCKED`).
- Step caching is keyed by **step name + index**. If a retry executes a different step at the same index, the worker returns an error. Prefer stable step order and put branching logic inside a step when possible.
- Outputs and inputs are stored as JSON TEXT, so return types must be serde‑serializable.
- Duplicate `#[workflow]` names (same `module_path::fn_name`) are detected at worker boot via `registry::assert_no_duplicates` and abort startup with a clear error.
- The worker honours `SIGINT` / `SIGTERM` by draining in‑flight workflows before exiting; no in‑flight workflow is orphaned mid‑step on a clean shutdown.
