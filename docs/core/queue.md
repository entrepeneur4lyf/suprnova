# Queue

Suprnova's queue subsystem dispatches background work, retries failures with
backoff, throttles overlapping or rate-limited handlers, batches jobs with
progress tracking, and chains sequential workflows. The shape mirrors
Laravel 13's queue API while diverging where Rust's async runtime makes a
better choice (Tokio for the worker loop, typed envelopes instead of PHP
serialization, dedicated outcome enum for middleware control).

## Quick start

```rust
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use suprnova::{error::FrameworkError, queue::{Job, Queue}};

#[derive(Serialize, Deserialize)]
struct SendWelcomeEmail { user_id: i64 }

#[async_trait]
impl Job for SendWelcomeEmail {
    fn job_name() -> &'static str { "SendWelcomeEmail" }

    async fn handle(self) -> Result<(), FrameworkError> {
        // ... actual send
        Ok(())
    }
}

// Boot once
Queue::set_driver(std::sync::Arc::new(suprnova::queue::MemoryQueueDriver::new()));
suprnova::queue::worker::register_job::<SendWelcomeEmail>();

// Dispatch
Queue::push(SendWelcomeEmail { user_id: 42 }).await?;
```

A worker process drains the configured driver:

```rust
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use suprnova::queue::{Queue, worker::{WorkerConfig, run_worker}};

let driver = Queue::driver()?;
let cfg = WorkerConfig {
    visibility_timeout: Duration::from_secs(60),
    poll_interval: Duration::from_millis(100),
    max_jobs: None,
};
let shutdown = CancellationToken::new();
run_worker(driver, cfg, shutdown).await;
```

## Drivers

Four drivers ship in-tree. Pick via `QUEUE_DRIVER` env or by calling
`Queue::set_driver(...)` programmatically.

| Driver | Use for | Strengths |
| --- | --- | --- |
| `MemoryQueueDriver` | tests, single-process apps | DelayQueue-backed `available_at`, tokio virtual clock compatible |
| `RedisQueueDriver` | production fan-out | consumer groups + auto-claim + ZSET-backed delayed jobs |
| `DatabaseQueueDriver` | single-DB apps | `FOR UPDATE SKIP LOCKED` on Postgres/MySQL, write-blocking on SQLite |
| `SyncQueueDriver` | dev, CI | runs the handler inline on `push`, no worker |
| `NullQueueDriver` | testing wrappers | drops every push without running |

Configure via env:

```bash
QUEUE_DRIVER=redis
QUEUE_REDIS_URL=redis://127.0.0.1:6379
QUEUE_REDIS_STREAM=suprnova-queue
QUEUE_REDIS_GROUP=default
QUEUE_REDIS_CONSUMER=consumer-1
QUEUE_VISIBILITY_TIMEOUT_SECS=60
```

## Push variants

| Method | Behavior |
| --- | --- |
| `Queue::push(job)` | enqueue immediately |
| `Queue::push_later(job, at)` | available at a specific timestamp |
| `Queue::later(delay, job)` | available after `delay` from now |
| `Queue::push_unique(job)` | dedupe by `J::unique_id` within `J::unique_for` |
| `Queue::push_unique_later(job, at)` | unique + scheduled |
| `Queue::later_unique(delay, job)` | unique + delayed |
| `Queue::bulk(vec![job1, job2, ...])` | push every job (driver may use a native bulk path) |

## Backoff schedules

Implement `Job::backoff()` to override the default exponential schedule:

```rust
fn backoff() -> BackoffSchedule {
    BackoffSchedule::Sequence { secs: vec![5, 15, 60, 300] }
}
```

Variants: `Fixed { secs }`, `Exponential { base_secs, cap_secs, jitter_ratio }`,
`Sequence { secs }`. The default is exponential 2s → 5min with ±25% jitter.

## Job middleware

Wrap the handler in cross-cutting middleware. Suprnova ships five:

| Middleware | Behavior |
| --- | --- |
| `WithoutOverlapping` | hold a `Cache::lock` for the duration; release-with-delay on contention |
| `RateLimited` | gate on `RateLimiter` budget; release until window resets |
| `ThrottlesExceptions` | rate-limit on consecutive failures, not requests |
| `Skip::when(cond)` / `Skip::unless(cond)` | drop the job when the condition is met |
| `FailOnException` | promote matching errors to permanent failures (no retry) |
| `SkipIfBatchCancelled` | drop the job if its owning batch was cancelled |

Register on the `Job` impl:

```rust
fn middleware() -> Vec<std::sync::Arc<dyn JobMiddleware>> {
    vec![
        std::sync::Arc::new(WithoutOverlapping::new("user-42").expire_after(Duration::from_secs(120))),
        std::sync::Arc::new(RateLimited::new(10, Duration::from_secs(60)).by("send-mail")),
    ]
}
```

### The release-without-burning-attempt contract

Middleware returns a `JobOutcome` rather than `Result<()>`. Four variants:

- `JobOutcome::Completed` — handler ran, ack.
- `JobOutcome::Released { delay }` — re-enqueue after `delay` **without**
  incrementing `attempts`. Used by `WithoutOverlapping`, `RateLimited`. The
  worker emits a `JobReleased` event (distinct from
  `JobReleasedAfterException`).
- `JobOutcome::Failed { reason }` — dead-letter now, persist to failed-jobs
  store, do not retry.
- `JobOutcome::Deleted` — drop the reservation without dead-letter. Used by
  `Skip`.

This is the contract that makes "throttled because the bucket was full" feel
different from "failed because the handler errored" in retry accounting.

## Lifecycle events

Workers emit Laravel-shape lifecycle events through the
[`Event`](events.md) facade. Listeners get the envelope's identity (id,
job_name, attempts, max_tries, connection), not the typed job instance —
the worker is type-erased over JSON payloads. Errors travel as a `String`
since `FrameworkError` doesn't derive `Clone`.

| Event | Fires when |
| --- | --- |
| `JobQueueing` | before the envelope hits the driver |
| `JobQueued` | after the driver accepts |
| `JobProcessing` | worker popped, about to dispatch |
| `JobProcessed` | handler returned Ok |
| `JobAttempted` | every terminal settlement (success, fail, timeout) |
| `JobExceptionOccurred` | handler returned Err, will retry |
| `JobReleasedAfterException` | retry-after-error re-enqueue happened |
| `JobReleased` | middleware-driven release (no failure) |
| `JobFailed` | dead-lettered |
| `JobTimedOut` | per-attempt timeout exceeded |
| `Looping` | every loop iteration |
| `WorkerStarting` / `WorkerStopping` | once per worker lifetime |
| `WorkerInterrupted` | restart signal observed |

Subscribe with the normal `Event::listen` API.

## Failed-jobs storage

Dead-lettered jobs land in the configured `FailedJobStore`:

```rust
use suprnova::queue::{Queue, MemoryFailedJobStore};

Queue::set_failed_store(std::sync::Arc::new(MemoryFailedJobStore::new()));

// Later, in admin tooling:
let store = Queue::failed_store().unwrap();
for record in store.all().await? {
    println!("{} failed: {}", record.job_name, record.exception);
}
store.forget(some_id).await?;
store.flush(None).await?;
```

Two production backends: `MemoryFailedJobStore` (in-process, lost on
restart) and `DatabaseFailedJobStore` (persists to a `failed_jobs` table
via SeaORM). `NullFailedJobStore` discards every record for environments
that don't need persistence.

### `failed_jobs` schema

The `DatabaseFailedJobStore` expects this table (managed by your migrations):

```sql
CREATE TABLE failed_jobs (
    id              TEXT PRIMARY KEY,
    connection      TEXT NOT NULL,
    queue           TEXT NOT NULL,
    job_name        TEXT NOT NULL,
    envelope_json   TEXT NOT NULL,
    exception       TEXT NOT NULL,
    failed_at       INTEGER NOT NULL
);
CREATE INDEX idx_failed_jobs_failed_at ON failed_jobs(failed_at);
```

## Queued batches

Dispatch a group of jobs with progress tracking and completion callbacks:

```rust
use std::sync::Arc;
use suprnova::queue::{Queue, MemoryBatchRepository, batch::register_callback};

Queue::set_batch_repository(Arc::new(MemoryBatchRepository::new()));

// Register named callbacks at boot.
register_callback(Arc::new(NotifySuccess::new("send-summary-email")));
register_callback(Arc::new(NotifyFailure::new("page-on-fail")));

let id = Queue::batch()
    .name("import-users")
    .add(ImportUser { id: 1 })
    .add(ImportUser { id: 2 })
    .add(ImportUser { id: 3 })
    .then("send-summary-email")
    .catch("page-on-fail")
    .finally("cleanup-temp-tables")
    .dispatch()
    .await?;

// Inspect progress later:
let repo = Queue::batch_repository().unwrap();
let snap = repo.find(&id).await?.unwrap();
println!("{}/{} jobs done ({}%)", snap.processed_jobs(), snap.total_jobs, snap.progress());
```

Each worker decrements `pending_jobs` on success and bumps `failed_jobs` on
dead-letter. When `pending_jobs` hits zero, the worker calls registered
`then`/`catch`/`finally` callbacks. By default the first failure cancels
the batch; `.allow_failures()` keeps remaining jobs going.

### Batch options

| Option | Builder method | Effect |
| --- | --- | --- |
| Allow failures | `.allow_failures()` | continue scheduling after a job fails |
| Then callback | `.then(name)` | runs on every-job-success |
| Catch callback | `.catch(name)` | runs on first failure |
| Finally callback | `.finally(name)` | runs after batch settles either way |
| Skip cancelled | `SkipIfBatchCancelled` middleware on the job | drop remaining jobs when batch is cancelled |

### `BatchCallback` impl

```rust
use async_trait::async_trait;
use suprnova::queue::{Batch, BatchCallback};
use suprnova::error::FrameworkError;

pub struct SendSummary;

#[async_trait]
impl BatchCallback for SendSummary {
    fn name(&self) -> &'static str { "send-summary-email" }
    async fn handle(&self, batch: Batch, error: Option<String>) -> Result<(), FrameworkError> {
        let subject = match error {
            Some(_) => format!("Batch {} failed", batch.name),
            None    => format!("Batch {} done — {} jobs", batch.name, batch.total_jobs),
        };
        // ... send mail
        Ok(())
    }
}
```

Register at boot with `batch::register_callback(Arc::new(SendSummary))`.

## Queued chains

Sequential workflows where each link runs only after the prior one's
handler ack's:

```rust
Queue::chain()
    .add(GenerateReport { id: 99 })?
    .add(UploadToBucket { id: 99 })?
    .add(NotifyOwner { id: 99 })?
    .dispatch()
    .await?;
```

The first envelope is pushed immediately; the remaining links travel on
its `chain_remaining` payload field. On every successful settlement the
worker reads the tail, dispatches the next link, and repeats. A failure
breaks the chain — subsequent links are never enqueued.

## Introspection

```rust
Queue::size().await?;            // total
Queue::pending_size().await?;    // available_at <= now, not reserved
Queue::delayed_size().await?;    // available_at > now
Queue::reserved_size().await?;   // currently popped, not yet acked
Queue::clear().await?;           // drop every envelope, returns the count
```

The trait `QueueDriver` declares defaults for `size`/`pending_size`/
`reserved_size`/`delayed_size`/`clear`; `MemoryQueueDriver` and
`DatabaseQueueDriver` implement them natively. `RedisQueueDriver` does
not yet implement size/clear and returns an "unsupported" error — use the
admin redis-cli for those for now.

## Worker restart signal

`php artisan queue:restart` translates to:

```rust
Queue::restart().await?;
```

The signal lives in `Cache` (millisecond timestamp). Workers poll once per
loop and exit cleanly when the timestamp is newer than their start time.
Pair with a supervisor (systemd, Kubernetes, the `supervisor` module) so a
fresh worker picks up where the previous one stopped.

## Typed errors

`MaxAttemptsExceeded`, `TimeoutExceeded`, and `ManuallyFailed` mirror
Laravel's `MaxAttemptsExceededException` / `TimeoutExceededException` /
`ManuallyFailedException`. Workers attach the relevant cause to the
dead-letter `JobFailed` event so listeners can pattern-match.

## Connection naming

Workers tag every lifecycle event with a connection name. By default this
is the driver's `name()` (e.g. `"memory"`, `"redis"`, `"database"`). Apps
that run multiple connections at once can override:

```rust
Queue::set_connection_name("orders-redis");
```

## Tests

`Queue::fake()` semantics live in `queue::testing`:

```rust
let _guard = suprnova::queue::testing::install_fake();
my_code_that_dispatches_jobs().await;
suprnova::queue::testing::assert_pushed::<SendWelcomeEmail>(|j| j.user_id == 42);
```

The fake guard serializes parallel tests via a process-wide mutex; it
captures `(payload, available_at)` so `assert_pushed_later` can pin the
scheduled timestamp.
