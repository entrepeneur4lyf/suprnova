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

## Notes

- The worker requires **Postgres** (uses `FOR UPDATE SKIP LOCKED`).
- Steps should be deterministic and side‑effect safe since retries resume at the last failed step.
- Step caching is keyed by **step name + index**. If a retry executes a different step at the same index, the worker returns an error. Prefer stable step order and put branching logic inside a step when possible.
- Outputs and inputs are stored as JSON TEXT, so return types must be serde‑serializable.
