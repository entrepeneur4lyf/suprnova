# Queue

The `Queue` facade dispatches background work to a driver and lets a separate
worker process drain it: HTTP handlers return fast, the heavy lifting runs
behind the scenes. Reach for it whenever a request would otherwise block on
something that can be done later — sending mail, hitting a webhook, generating
a report. Pair with [`Bus`](bus.md) when you want the work to run *now* in the
current task and return a typed result; pair with [`Events`](events.md) when
you want one signal to fan out to many listeners.

## Quick start

Define a job, register it once at boot, push it:

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
        // … actually send the mail
        Ok(())
    }
}

// Boot once (the worker process and the dispatch process both need this).
Queue::set_driver(std::sync::Arc::new(suprnova::queue::MemoryQueueDriver::new()));
suprnova::queue::worker::register_job::<SendWelcomeEmail>();

// Push from a handler:
Queue::push(SendWelcomeEmail { user_id: 42 }).await?;
```

A worker process drains the configured driver until cancelled:

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

In a scaffolded app, the worker is started by the binary's `queue:work`
subcommand — `cargo run -- queue:work` — which runs the same bootstrap your
HTTP server does, so observers and listeners registered in `bootstrap()`
fire identically for inserts from a queue handler.

## Drivers

Five drivers ship in-tree. Configure via `QUEUE_DRIVER` env or by calling
`Queue::set_driver(...)` programmatically.

| Driver | Use for | Strengths |
| --- | --- | --- |
| `MemoryQueueDriver` | tests, single-process apps | `tokio::time::DelayQueue` for `available_at`, virtual-clock compatible |
| `RedisQueueDriver` | production fan-out | consumer groups + `XAUTOCLAIM` + ZSET-backed delayed jobs |
| `DatabaseQueueDriver` | single-DB apps | `FOR UPDATE SKIP LOCKED` on Postgres/MySQL, `BEGIN`-serialised on SQLite |
| `SyncQueueDriver` | dev, CI | runs the handler inline on `push`, no worker |
| `NullQueueDriver` | testing wrappers | drops every push without running |

`Queue::bootstrap_from_env()` reads `QUEUE_DRIVER` and wires the matching
driver; `Queue::bootstrap_default()` always wires the memory driver. The
server boot path calls one of these for you — most apps only configure via
env.

### Environment configuration

```bash
QUEUE_DRIVER=redis
QUEUE_REDIS_URL=redis://127.0.0.1:6379
QUEUE_REDIS_STREAM=suprnova-queue
QUEUE_REDIS_GROUP=default
QUEUE_REDIS_CONSUMER=consumer-1
QUEUE_VISIBILITY_TIMEOUT_SECS=60

# Database driver — DB::init() must run first
QUEUE_DRIVER=database
QUEUE_DB_TABLE=jobs
```

The database driver validates `QUEUE_DB_TABLE` as a SQL identifier at
construction, so a malformed env value fails boot rather than reaching SQL
composition. Redis uses sea-streamer-redis under the hood with
`AutoCommit::Disabled`; the visibility timeout is fixed at consumer-group
construction time, so the per-pop `visibility_timeout` argument is ignored
on Redis (a documented divergence from the trait contract imposed by
Redis Streams).

### Why Suprnova diverges

Laravel routes every queueable through the Bus, distinguishing
`ShouldQueue` jobs at dispatch time. Suprnova splits the two: `Bus` for
synchronous work that returns a typed result, `Queue` for asynchronous
work that survives a process crash. PHP needs the implicit routing
because its request-per-process model makes "do this later, in another
process" hard to model otherwise. Tokio doesn't — explicit `Bus::dispatch`
vs `Queue::push` is clearer, faster, and surfaces the durability choice
at the call site. See [`bus.md`](bus.md) for the side-by-side.

## Push variants

Every push variant takes a typed `J: Job` value and returns when the
envelope is committed to the driver — not when the handler runs.

| Method | Behavior |
| --- | --- |
| `Queue::push(job)` | enqueue immediately |
| `Queue::push_later(job, at)` | available at a specific `DateTime<Utc>` |
| `Queue::later(delay, job)` | available after `delay` from now |
| `Queue::push_unique(job)` | dedupe by `J::unique_id` within `J::unique_for`, returns `Ok(true)` for fresh, `Ok(false)` for duplicate |
| `Queue::push_unique_later(job, at)` | unique + scheduled |
| `Queue::later_unique(delay, job)` | unique + delayed |
| `Queue::bulk(vec![job1, job2, ...])` | push every job (driver may use a native bulk path) |

`push_unique` requires the cache layer to be bootstrapped — the dedupe
lock lives in [`Cache`](cache.md) via
[`Idempotency::commit_on_success`](idempotency.md). A failed push releases
the dedupe key so the caller can retry; a successful push holds it for
`J::unique_for` seconds. The job must override `Job::unique_id(&self)` to
return `Some(id)` — `None` returns an internal error.

## Job configuration

Override `Job`'s associated functions to tune behavior per impl:

```rust
use std::time::Duration;
use suprnova::queue::{BackoffSchedule, JobMiddleware};

#[async_trait]
impl Job for SendWelcomeEmail {
    fn job_name() -> &'static str { "SendWelcomeEmail" }

    async fn handle(self) -> Result<(), FrameworkError> { /* … */ Ok(()) }

    fn max_tries() -> u32 { 5 }                            // default: 3
    fn timeout() -> Option<Duration> { Some(Duration::from_secs(30)) }
    fn fail_on_timeout() -> bool { false }                 // default: false (timeout retries)
    fn backoff() -> BackoffSchedule {
        BackoffSchedule::Sequence { secs: vec![5, 15, 60, 300] }
    }
    fn unique_id(&self) -> Option<String> {
        Some(format!("welcome:{}", self.user_id))
    }
    fn unique_for() -> Duration { Duration::from_secs(600) }  // default: 5 minutes
    fn middleware() -> Vec<std::sync::Arc<dyn JobMiddleware>> {
        vec![/* see "Job middleware" below */]
    }
}
```

### Backoff schedules

| Variant | Behavior |
| --- | --- |
| `Fixed { secs }` | constant per-attempt delay |
| `Exponential { base_secs, cap_secs, jitter_ratio }` | `min(base * 2^(attempts-1), cap)` × random in `[1±jitter]` |
| `Sequence { secs }` | one entry per attempt; the last entry repeats once exhausted |

The default is `Exponential { base_secs: 2, cap_secs: 300, jitter_ratio: 0.25 }`
— 2 seconds to 5 minutes with ±25% jitter.

## Job middleware

Six middleware ship in-tree, all mirroring `Illuminate\Queue\Middleware\*`:

| Middleware | Behavior |
| --- | --- |
| `WithoutOverlapping` | hold a `Cache::lock` for the duration; release-with-delay on contention |
| `RateLimited` | gate on `RateLimiter` budget; release until the window resets |
| `ThrottlesExceptions` | rate-limit on consecutive *failures*, not requests |
| `Skip::when(cond)` / `Skip::unless(cond)` | drop the job when the condition is met |
| `FailOnException` | promote matching errors to permanent failures (no retry) |
| `SkipIfBatchCancelled` | drop the job if its owning batch was cancelled |

Wire them on the `Job` impl:

```rust
use std::sync::Arc;
use std::time::Duration;
use suprnova::queue::{JobMiddleware, RateLimited, WithoutOverlapping};

fn middleware() -> Vec<Arc<dyn JobMiddleware>> {
    vec![
        Arc::new(
            WithoutOverlapping::new("user-42")
                .expire_after(Duration::from_secs(120))
        ),
        Arc::new(
            RateLimited::new(10, Duration::from_secs(60))
                .by("send-mail")
        ),
    ]
}
```

`WithoutOverlapping` and `RateLimited` need the cache subsystem booted
(`Cache::init` or `App::bind::<dyn CacheStore>(...)` at startup).

### The release-without-burning-attempt contract

Middleware returns a `JobOutcome` rather than `Result<()>`. Four variants:

- `JobOutcome::Completed` — handler ran, ack.
- `JobOutcome::Released { delay }` — re-enqueue after `delay` **without**
  incrementing `attempts`. Used by `WithoutOverlapping`, `RateLimited`. The
  worker decrements the local copy back to pre-dispatch, acks the original
  reservation, then pushes a copy with `available_at` shifted by `delay` —
  at-least-once, but the re-delivery on a crash between ack and push is
  benign (another release attempt) and strictly better than incrementing
  attempts on every contention cycle.
- `JobOutcome::Failed { reason }` — dead-letter now, persist to the
  failed-jobs store, do not retry.
- `JobOutcome::Deleted` — drop the reservation without dead-letter. Used
  by `Skip`. If the job belonged to a batch, the batch's `pending_jobs`
  decrements anyway so callbacks can fire.

This contract is what makes "throttled because the bucket was full" feel
different from "failed because the handler errored" in retry accounting,
metrics, and lifecycle events.

## Lifecycle events

Workers emit Laravel-shape lifecycle events through the
[`Event`](events.md) facade. Listeners get the envelope's identity (`id`,
`job_name`, `attempts`, `max_tries`, `connection`), not the typed job
instance — the worker is type-erased over JSON payloads. Errors travel
as a `String` since `FrameworkError` doesn't derive `Clone`.

| Event | Fires when |
| --- | --- |
| `JobQueueing` | before the envelope hits the driver |
| `JobQueued` | after the driver accepts |
| `JobProcessing` | worker popped, about to dispatch |
| `JobProcessed` | handler returned `Ok` |
| `JobAttempted` | every terminal settlement (success, fail, timeout) |
| `JobExceptionOccurred` | handler returned `Err`, will retry |
| `JobReleasedAfterException` | retry-after-error re-enqueue happened |
| `JobReleased` | middleware-driven release (no failure) |
| `JobFailed` | dead-lettered |
| `JobTimedOut` | per-attempt timeout exceeded |
| `Looping` | every loop iteration (before the pop) |
| `WorkerStarting` / `WorkerStopping` | once per worker lifetime |
| `WorkerInterrupted` | `Queue::restart()` signal observed |

Subscribe with the normal `Event::listen` API. Events are best-effort —
`Event::dispatch` with no listeners is a no-op `Ok(())`, so workers in
deployments without `Event::init()` pay nothing.

## Failed-jobs storage

Dead-lettered jobs land in the configured `FailedJobStore`:

```rust
use std::sync::Arc;
use suprnova::queue::{Queue, MemoryFailedJobStore};

Queue::set_failed_store(Arc::new(MemoryFailedJobStore::new()));

// In admin tooling:
let store = Queue::failed_store().unwrap();
for record in store.all().await? {
    println!("{} failed: {}", record.job_name, record.exception);
}
store.forget(some_id).await?;
store.flush(None).await?;
```

Three backends:

- `MemoryFailedJobStore` — in-process `Vec`, lost on restart.
- `DatabaseFailedJobStore` — persists to a `failed_jobs` table via SeaORM.
- `NullFailedJobStore` — discards every record. Mirrors Laravel's
  `NullFailedJobProvider`.

### Retrying

```rust
use uuid::Uuid;

// Single record — false if the id wasn't in the store.
Queue::retry_failed(some_id).await?;

// Bulk — optional cutoff (only retry records older than `before`).
let count = Queue::retry_all_failed(None).await?;
```

`retry_failed` loads the envelope, resets `attempts`, `available_at`, and
the `idempotency_key`, pushes through the configured driver, then deletes
the failed-job record. Mirrors `php artisan queue:retry <id>` plus
`queue:flush` semantics (each retried envelope is pushed AND removed
from the store).

### `failed_jobs` schema

The `DatabaseFailedJobStore` expects this table (managed by your
migrations):

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

The `table` argument to `DatabaseFailedJobStore::new` is validated as a
SQL identifier at construction.

## Queued batches

Dispatch a group of jobs with progress tracking and completion callbacks:

```rust
use std::sync::Arc;
use suprnova::queue::{Queue, MemoryBatchRepository, batch::register_callback};

Queue::set_batch_repository(Arc::new(MemoryBatchRepository::new()));

// Register named callbacks at boot.
register_callback(Arc::new(SendSummary));
register_callback(Arc::new(PageOnFail));

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

Each worker decrements `pending_jobs` on success and bumps `failed_jobs`
on dead-letter. When `pending_jobs` hits zero, the worker fires
registered `then`/`catch`/`finally` callbacks. By default the first
failure cancels the batch; `.allow_failures()` keeps remaining jobs going.

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
        // … send mail
        Ok(())
    }
}
```

Register at boot with `batch::register_callback(Arc::new(SendSummary))`.
Callbacks are keyed by `name()` — the batch's options store callback
names, so a process restart picks up registered callbacks by lookup
instead of trying to deserialize a closure (Rust closures don't
serialize).

## Queued chains

Sequential workflows where each link runs only after the previous one's
handler acks:

```rust
Queue::chain()
    .add(GenerateReport { id: 99 })?
    .add(UploadToBucket { id: 99 })?
    .add(NotifyOwner { id: 99 })?
    .dispatch()
    .await?;
```

The first envelope is pushed immediately; the rest travel on its
`chain_remaining` payload field. On every successful settlement the
worker pops the next entry and dispatches it. A failure breaks the
chain — subsequent links are never enqueued.

## Introspection

```rust
Queue::size().await?;            // total
Queue::pending_size().await?;    // available_at <= now, not reserved
Queue::delayed_size().await?;    // available_at > now
Queue::reserved_size().await?;   // currently popped, not yet acked
Queue::clear().await?;           // drop every envelope, returns the count
Queue::driver_name()?;           // configured driver name for logs / admin
```

The `QueueDriver` trait declares defaults for `size` / `pending_size` /
`reserved_size` / `delayed_size` / `clear`; `MemoryQueueDriver` and
`DatabaseQueueDriver` implement them natively. `RedisQueueDriver`
returns an "unsupported" error for `size` / `clear` — use the admin
redis-cli for those.

## Worker restart signal

`php artisan queue:restart` translates to:

```rust
Queue::restart().await?;
```

The signal lives in `Cache` as a millisecond timestamp. Workers poll
once per loop and exit cleanly when the timestamp is newer than their
start time. Pair with a supervisor (systemd, Kubernetes, the
`supervisor` module) so a fresh worker picks up where the previous one
stopped.

## Graceful shutdown

The worker's `CancellationToken` fires at the next pop boundary, never
mid-dispatch. A handler that's already been popped runs to completion
(bounded by its own `Job::timeout()` if set) before the worker exits.
That means in-flight side effects don't get torn mid-stride, but a
SIGTERM can take up to the per-job timeout to drain. Set
`WorkerConfig::max_jobs` for a periodic-restart strategy on long-lived
workers; the worker exits cleanly after that many settlements regardless
of outcome.

## Settlement metrics

The worker emits a `queue.settlement.failures` counter via [`Metrics`](observability.md) on every ack/nack failure. Attributes: `operation`
(`"ack"` | `"nack"`), `driver` (the configured driver's name), `job`
(the job_name), `outcome` (`"success"`, `"dead_letter"`, `"retry"`,
`"deleted"`, `"timeout_dead_letter"`, `"timeout_retry"`, `"released"`).

A non-zero rate here means at-least-once delivery may re-deliver a
successful side effect or lose attempt accounting — alert on it
explicitly.

## Typed errors

`MaxAttemptsExceeded`, `TimeoutExceeded`, and `ManuallyFailed` mirror
Laravel's `MaxAttemptsExceededException` / `TimeoutExceededException` /
`ManuallyFailedException`. The worker attaches the relevant cause to
the dead-letter `JobFailed` event so listeners can pattern-match instead
of substring-searching the error message.

## Connection naming

Workers tag every lifecycle event with a connection name. By default
this is the driver's `name()` (e.g. `"memory"`, `"redis"`, `"database"`).
Apps that run multiple connections at once can override:

```rust
Queue::set_connection_name("orders-redis");
```

## Testing

`Queue::fake()` semantics live in `queue::testing`:

```rust
let _guard = suprnova::queue::testing::install_fake();
my_code_that_dispatches_jobs().await;

suprnova::queue::testing::assert_pushed::<SendWelcomeEmail>(|j| j.user_id == 42);

// For delayed dispatches, pin the scheduled timestamp:
suprnova::queue::testing::assert_pushed_later::<SendWelcomeEmail>(|j, at| {
    j.user_id == 42 && at > chrono::Utc::now()
});
```

The fake guard serialises parallel tests via a process-wide mutex; it
captures `(payload, available_at)` per push and clears on `Drop`. In
fake mode, `push_unique` always records the push as fresh — dedupe is
irrelevant when no driver is wired.

## Idempotency is the worker's contract with you

Redis-backed queue drivers can't make `nack` atomic — `XADD` and `XACK`
are separate commands. A crash between them re-delivers the message via
`XAUTOCLAIM`. In-memory and database drivers are exactly-once-per-attempt,
but the worker loop doesn't distinguish drivers, so **every job handler
in a production deployment must be idempotent**.

For typical command-style jobs, wrap the handler body in
[`Idempotency::once`](idempotency.md) or
[`Idempotency::commit_on_success`](idempotency.md) keyed by a stable
per-operation key (entity id, caller-supplied request id, etc.). When a
retry must return the *original* outcome rather than skip re-execution,
use `Idempotency::remember`, which records the success value and
replays it on later deliveries.

## Next

- [Bus](bus.md) — synchronous dispatcher with typed results
- [Events](events.md) — pub/sub fan-out
- [Idempotency](idempotency.md) — the contract handlers honour for at-least-once delivery
- [Cache](cache.md) — backs `push_unique`, `WithoutOverlapping`, `RateLimited`
- [Mocking](mocking.md) — every fake guard, including `Queue::fake`
