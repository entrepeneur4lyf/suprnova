//! Queue subsystem: facade, drivers, envelope, worker.

pub mod batch;
pub mod chain;
pub mod database;
pub mod driver;
pub mod envelope;
pub mod errors;
pub mod events;
pub mod failed;
pub mod job;
pub mod memory;
pub mod middleware;
pub mod null;
pub mod outcome;
pub mod redis;
pub mod retry;
pub mod sync;
pub mod testing;
pub mod worker;

pub use batch::{
    Batch, BatchCallback, BatchOptions, BatchRepository, MemoryBatchRepository, PendingBatch,
    UpdatedBatchJobCounts,
};
pub use chain::{ChainLink, PendingChain};
pub use database::DatabaseQueueDriver;
pub use driver::{QueueDriver, Reservation, ReservationToken};
pub use envelope::{CURRENT_SCHEMA_VERSION, Envelope, EnvelopeError};
pub use errors::{ManuallyFailed, MaxAttemptsExceeded, TimeoutExceeded};
pub use failed::{
    DatabaseFailedJobStore, FailedJob, FailedJobStore, MemoryFailedJobStore, NullFailedJobStore,
};
pub use job::{BackoffSchedule, Job};
pub use memory::MemoryQueueDriver;
pub use middleware::{
    FailOnException, JobMiddleware, Next as JobMiddlewareNext, RateLimited, Skip,
    SkipIfBatchCancelled, ThrottlesExceptions, WithoutOverlapping,
};
pub use null::NullQueueDriver;
pub use outcome::JobOutcome;
pub use redis::RedisQueueDriver;
pub use sync::SyncQueueDriver;

use crate::error::FrameworkError;
use crate::lock;
use chrono::Utc;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

static DRIVER: RwLock<Option<Arc<dyn QueueDriver>>> = RwLock::new(None);

/// Process-wide name for the current queue connection. Carried in queue
/// lifecycle events so listeners can distinguish driver instances when an
/// app runs multiple connections at once.
static CONNECTION_NAME: RwLock<Option<String>> = RwLock::new(None);

/// Cache key for the cross-worker restart signal. Worker checks the
/// timestamp every loop iteration; if it's newer than the worker's
/// startup time, the worker exits.
const RESTART_SIGNAL_KEY: &str = "queue:restart-signal";

/// `Queue` facade.
///
/// Configure once at boot via `Queue::set_driver(...)` (or one of the
/// `Queue::use_*` helpers added in later tasks). In tests, install
/// `testing::install_fake()` and assert with `testing::assert_pushed`.
pub struct Queue;

impl Queue {
    /// Push a typed job. Returns when the envelope is committed to the
    /// driver (NOT when the job runs).
    pub async fn push<J: Job>(job: J) -> Result<(), FrameworkError> {
        let now = Utc::now();
        if testing::is_active() {
            return testing::record::<J>(&job, now);
        }
        let env = envelope_for::<J>(&job, now)?;
        let _ = crate::events::EventFacade::dispatch(events::JobQueueing {
            job_name: J::job_name().into(),
            connection: Self::connection_name(),
        })
        .await;
        let drv = current_driver()?;
        let env_id = env.id;
        drv.push(env).await?;
        let _ = crate::events::EventFacade::dispatch(events::JobQueued {
            id: env_id,
            job_name: J::job_name().into(),
            connection: Self::connection_name(),
        })
        .await;
        Ok(())
    }

    /// Push a typed job available at `available_at`. Driver is responsible
    /// for honoring the timestamp.
    pub async fn push_later<J: Job>(
        job: J,
        available_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), FrameworkError> {
        if testing::is_active() {
            return testing::record::<J>(&job, available_at);
        }
        let env = envelope_for::<J>(&job, available_at)?;
        let _ = crate::events::EventFacade::dispatch(events::JobQueueing {
            job_name: J::job_name().into(),
            connection: Self::connection_name(),
        })
        .await;
        let drv = current_driver()?;
        let env_id = env.id;
        drv.push(env).await?;
        let _ = crate::events::EventFacade::dispatch(events::JobQueued {
            id: env_id,
            job_name: J::job_name().into(),
            connection: Self::connection_name(),
        })
        .await;
        Ok(())
    }

    /// Convenience: push with a delay from `now`.
    pub async fn later<J: Job>(delay: std::time::Duration, job: J) -> Result<(), FrameworkError> {
        let available_at = Utc::now()
            + chrono::Duration::from_std(delay)
                .map_err(|e| FrameworkError::internal(format!("delay overflow: {e}")))?;
        Self::push_later(job, available_at).await
    }

    /// Push a typed job, but only if no job with the same
    /// `(job_name, J::unique_id(&job))` was successfully enqueued in the
    /// last [`Job::unique_for`]. Returns `Ok(true)` when the job was
    /// pushed, `Ok(false)` when it was suppressed as a duplicate.
    ///
    /// Backed by [`Idempotency::commit_on_success`](crate::idempotency::Idempotency::commit_on_success):
    /// a push failure releases the dedupe key so the caller can retry; a
    /// successful push holds the key for `unique_for` to gate re-submissions.
    ///
    /// Requires the cache layer to be bootstrapped (the dedupe lock lives
    /// in [`Cache`](crate::cache::Cache)). Returns an internal error if
    /// `J::unique_id(&job)` returns `None`.
    pub async fn push_unique<J: Job>(job: J) -> Result<bool, FrameworkError> {
        Self::push_unique_at::<J>(job, Utc::now()).await
    }

    /// `push_unique` variant that schedules the envelope for delivery at
    /// `available_at` (combines with the configured driver's delayed-job
    /// strategy: ZSET on Redis, `available_at` column on the database
    /// driver, virtual-clock DelayQueue on the memory driver).
    pub async fn push_unique_later<J: Job>(
        job: J,
        available_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, FrameworkError> {
        Self::push_unique_at::<J>(job, available_at).await
    }

    /// `push_unique` variant that takes a delay from now (the unique
    /// analogue of [`Queue::later`]).
    pub async fn later_unique<J: Job>(
        delay: std::time::Duration,
        job: J,
    ) -> Result<bool, FrameworkError> {
        let available_at = Utc::now()
            + chrono::Duration::from_std(delay)
                .map_err(|e| FrameworkError::internal(format!("delay overflow: {e}")))?;
        Self::push_unique_at::<J>(job, available_at).await
    }

    /// Common path for the three `*_unique*` entrypoints — builds the
    /// dedupe key, runs the enqueue under `Idempotency::commit_on_success`,
    /// and reports `true` for `Fresh`, `false` for `Duplicate`.
    async fn push_unique_at<J: Job>(
        job: J,
        available_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, FrameworkError> {
        if testing::is_active() {
            // In fake mode, dedupe is irrelevant — record and report fresh.
            testing::record::<J>(&job, available_at)?;
            return Ok(true);
        }
        let id = job.unique_id().ok_or_else(|| {
            FrameworkError::internal(
                "Queue::push_unique requires Job::unique_id(&self) to return Some(...)",
            )
        })?;
        let ttl = J::unique_for();
        let key = format!("queue-unique:{}:{}", J::job_name(), id);

        let outcome =
            crate::idempotency::Idempotency::commit_on_success(&key, ttl, move || async move {
                let mut env = envelope_for::<J>(&job, available_at)?;
                env.idempotency_key = Some(id);
                let drv = current_driver()?;
                drv.push(env).await
            })
            .await?;

        Ok(matches!(outcome, crate::idempotency::Idempotent::Fresh(())))
    }

    /// Push every job in `jobs` onto the queue. Mirrors Laravel's
    /// `Queue::bulk($jobs, $data, $queue)`. Each job is encoded and
    /// committed via the driver's [`QueueDriver::bulk_push`] hook (with a
    /// serial-push default).
    pub async fn bulk<J: Job + Clone>(jobs: Vec<J>) -> Result<(), FrameworkError> {
        if testing::is_active() {
            let now = Utc::now();
            for j in jobs {
                testing::record::<J>(&j, now)?;
            }
            return Ok(());
        }
        let now = Utc::now();
        let mut envs = Vec::with_capacity(jobs.len());
        for j in jobs {
            envs.push(envelope_for::<J>(&j, now)?);
        }
        let drv = current_driver()?;
        drv.bulk_push(envs).await
    }

    /// Begin a queued batch builder. Mirrors `Bus::batch([...])`.
    ///
    /// Add jobs with `.add(job)`, register `then`/`catch`/`finally`
    /// callbacks by name, then `.dispatch()` to push every job through
    /// the configured driver under one batch id.
    pub fn batch() -> PendingBatch {
        PendingBatch::new()
    }

    /// Begin a queued chain builder. Mirrors `Bus::chain([...])`.
    pub fn chain() -> PendingChain {
        PendingChain::new()
    }

    /// Total envelopes currently held by the driver
    /// (pending + delayed + reserved).
    pub async fn size() -> Result<u64, FrameworkError> {
        current_driver()?.size().await
    }

    /// Envelopes whose `available_at <= now` and which are not reserved.
    pub async fn pending_size() -> Result<u64, FrameworkError> {
        current_driver()?.pending_size().await
    }

    /// Envelopes whose `available_at > now`.
    pub async fn delayed_size() -> Result<u64, FrameworkError> {
        current_driver()?.delayed_size().await
    }

    /// Envelopes currently held by an unfinished reservation.
    pub async fn reserved_size() -> Result<u64, FrameworkError> {
        current_driver()?.reserved_size().await
    }

    /// Drop every envelope on the configured driver. Returns the number
    /// of envelopes removed. Mirrors `Queue::clear($queue)`.
    pub async fn clear() -> Result<u64, FrameworkError> {
        current_driver()?.clear().await
    }

    /// Broadcast a restart signal to every worker on this connection.
    /// Workers poll the cache key once per loop and exit cleanly when
    /// the signal's timestamp is newer than their startup time. Mirrors
    /// Laravel's `php artisan queue:restart`.
    ///
    /// Requires the cache subsystem to be bootstrapped (the signal lives
    /// in [`Cache`](crate::cache::Cache)). The timestamp is stored in
    /// milliseconds so tightly-clustered `restart()` calls in tests are
    /// distinguishable.
    pub async fn restart() -> Result<(), FrameworkError> {
        let now = Utc::now().timestamp_millis();
        crate::cache::Cache::put(RESTART_SIGNAL_KEY, &now, None).await?;
        Ok(())
    }

    /// Read the latest restart-signal millisecond timestamp set by
    /// [`Queue::restart`]. Returns `None` when no signal has been issued.
    pub async fn restart_signal() -> Result<Option<i64>, FrameworkError> {
        crate::cache::Cache::get::<i64>(RESTART_SIGNAL_KEY).await
    }

    /// Replace the failed-jobs store (where the worker writes dead-lettered
    /// envelopes). Defaults to [`MemoryFailedJobStore`] when not set.
    pub fn set_failed_store(store: Arc<dyn FailedJobStore>) {
        failed::install(store);
    }

    /// Read the configured failed-jobs store. Returns `None` when none has
    /// been wired (in which case the worker still dead-letters via tracing
    /// but doesn't persist a record).
    pub fn failed_store() -> Option<Arc<dyn FailedJobStore>> {
        failed::current()
    }

    /// Re-enqueue a previously dead-lettered job by id. Loads the
    /// envelope from the configured [`FailedJobStore`], resets its
    /// `attempts`, `available_at`, and `idempotency_key`, pushes it
    /// through the configured driver, then deletes the failed-job
    /// record. Mirrors `php artisan queue:retry <id>`.
    ///
    /// Returns `Ok(true)` when the record was retried, `Ok(false)` when
    /// the id had no record in the store.
    pub async fn retry_failed(id: Uuid) -> Result<bool, FrameworkError> {
        let store = failed::current().ok_or_else(|| {
            FrameworkError::internal(
                "Queue::retry_failed requires a failed-jobs store; call \
                 Queue::set_failed_store(...) first",
            )
        })?;
        let Some(record) = store.find(id).await? else {
            return Ok(false);
        };
        let mut env = Envelope::from_json(&record.envelope_json)
            .map_err(|e| FrameworkError::internal(format!("retry_failed: decode envelope: {e}")))?;
        env.attempts = 0;
        env.available_at = Utc::now();
        env.idempotency_key = None;
        let drv = current_driver()?;
        drv.push(env).await?;
        store.forget(id).await?;
        Ok(true)
    }

    /// Re-enqueue every failed-job record (optionally only those older
    /// than `before`). Returns the number of records retried. Mirrors
    /// `php artisan queue:retry all` plus `queue:flush` semantics: each
    /// retried envelope is pushed AND removed from the store.
    pub async fn retry_all_failed(
        before: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<u64, FrameworkError> {
        let store = failed::current().ok_or_else(|| {
            FrameworkError::internal(
                "Queue::retry_all_failed requires a failed-jobs store; call \
                 Queue::set_failed_store(...) first",
            )
        })?;
        let records = store.all().await?;
        let drv = current_driver()?;
        let mut count: u64 = 0;
        for record in records {
            if let Some(cutoff) = before
                && record.failed_at >= cutoff
            {
                continue;
            }
            let Ok(mut env) = Envelope::from_json(&record.envelope_json) else {
                continue;
            };
            env.attempts = 0;
            env.available_at = Utc::now();
            env.idempotency_key = None;
            drv.push(env).await?;
            store.forget(record.id).await?;
            count += 1;
        }
        Ok(count)
    }

    /// Replace the batch repository. Defaults to [`MemoryBatchRepository`]
    /// on first use.
    pub fn set_batch_repository(repo: Arc<dyn BatchRepository>) {
        batch::install_repository(repo);
    }

    /// Read the configured batch repository.
    pub fn batch_repository() -> Option<Arc<dyn BatchRepository>> {
        batch::current_repository()
    }

    /// Set the connection name carried in queue lifecycle events. Defaults
    /// to the driver's `name()` if not overridden.
    pub fn set_connection_name(name: impl Into<String>) {
        if let Ok(mut g) = CONNECTION_NAME.write() {
            *g = Some(name.into());
        }
    }

    /// Resolve the connection name for events: explicit override → driver
    /// name → "default".
    pub fn connection_name() -> String {
        if let Ok(g) = CONNECTION_NAME.read()
            && let Some(n) = g.as_ref()
        {
            return n.clone();
        }
        current_driver()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "default".into())
    }

    /// Replace the registered driver. Primarily for boot-time wiring;
    /// in tests prefer `testing::install_fake()`.
    pub fn set_driver(driver: Arc<dyn QueueDriver>) {
        *lock::write(&DRIVER).unwrap_or_else(|e| panic!("{e}")) = Some(driver);
    }

    /// Return the registered driver's `name()` for observability (admin,
    /// `queue:work` startup log, debug). Returns the same `FrameworkError`
    /// that [`Queue::push`] would surface when no driver is registered.
    ///
    /// # Errors
    ///
    /// Returns [`FrameworkError::internal`] when the driver registry is
    /// poisoned, or when no driver has been wired (call
    /// [`bootstrap_default`] / [`bootstrap_from_env`] / [`Queue::set_driver`]
    /// at boot).
    pub fn driver_name() -> Result<&'static str, FrameworkError> {
        Ok(current_driver()?.name())
    }

    /// Return the registered driver as an `Arc<dyn QueueDriver>` so callers
    /// (workers, admin inspectors) can use it directly. Most app code should
    /// prefer the [`Queue::push`] facade.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Queue::driver_name`].
    pub fn driver() -> Result<Arc<dyn QueueDriver>, FrameworkError> {
        current_driver()
    }
}

pub(crate) fn current_driver() -> Result<Arc<dyn QueueDriver>, FrameworkError> {
    lock::read(&DRIVER)
        .map_err(|_| FrameworkError::internal("queue driver registry lock poisoned"))?
        .clone()
        .ok_or_else(|| {
            FrameworkError::internal(
                "queue driver not initialized; call Queue::set_driver(...) or install a test fake",
            )
        })
}

/// Wire the in-memory queue driver as the default. Idempotent.
pub async fn bootstrap_default() {
    if lock::read(&DRIVER)
        .map_err(|_| FrameworkError::internal("queue driver registry lock poisoned"))
        .map(|g| g.is_some())
        .unwrap_or(false)
    {
        return;
    }
    Queue::set_driver(Arc::new(memory::MemoryQueueDriver::new()));
}

/// Read `QUEUE_DRIVER` env and configure the matching driver. Falls back to the
/// in-memory default on any unrecognized value or when `QUEUE_DRIVER` is unset.
///
/// Unlike [`bootstrap_default`], this call **always replaces** the registered
/// driver — long-running processes (workers, tests) that re-invoke
/// `bootstrap_from_env` after `QUEUE_DRIVER` changes (or after an earlier
/// Redis/database boot) will pick up the new driver instead of being pinned to
/// the first one installed.
pub async fn bootstrap_from_env() -> Result<(), FrameworkError> {
    let driver = std::env::var("QUEUE_DRIVER").unwrap_or_else(|_| "memory".into());
    match driver.as_str() {
        "memory" => Queue::set_driver(Arc::new(memory::MemoryQueueDriver::new())),
        "redis" => {
            let url = std::env::var("QUEUE_REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
            let stream =
                std::env::var("QUEUE_REDIS_STREAM").unwrap_or_else(|_| "suprnova-queue".into());
            let group = std::env::var("QUEUE_REDIS_GROUP").unwrap_or_else(|_| "default".into());
            let consumer =
                std::env::var("QUEUE_REDIS_CONSUMER").unwrap_or_else(|_| "consumer-1".into());
            let visibility = std::time::Duration::from_secs(
                std::env::var("QUEUE_VISIBILITY_TIMEOUT_SECS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(60),
            );
            let d = redis::RedisQueueDriver::connect(&url, &stream, &group, &consumer, visibility)
                .await?;
            Queue::set_driver(Arc::new(d));
        }
        "database" => {
            let table = std::env::var("QUEUE_DB_TABLE").unwrap_or_else(|_| "jobs".into());
            // Requires DB::init() (or DB::init_with(...)) to have been called first.
            let db = crate::database::DB::connection().map_err(|e| {
                FrameworkError::internal(format!(
                    "QUEUE_DRIVER=database requires DB::init() to run first: {e}"
                ))
            })?;
            // DatabaseConnection is Arc-backed (SeaORM pool), so clone is cheap.
            // `new` validates QUEUE_DB_TABLE as a SQL identifier — a malformed
            // env value fails here instead of reaching SQL composition.
            let driver = database::DatabaseQueueDriver::new(db.inner().clone(), table)?;
            Queue::set_driver(Arc::new(driver));
        }
        other => {
            tracing::warn!(driver = %other, "unknown QUEUE_DRIVER, falling back to memory");
            Queue::set_driver(Arc::new(memory::MemoryQueueDriver::new()));
        }
    }
    Ok(())
}

fn envelope_for<J: Job>(
    job: &J,
    available_at: chrono::DateTime<chrono::Utc>,
) -> Result<Envelope, FrameworkError> {
    build_envelope::<J>(job, available_at)
}

/// Build an envelope for the typed job. Used by [`Queue::push`] and by
/// [`PendingBatch::add`] / [`PendingChain::add`]. `pub(crate)` because
/// external code goes through the facade.
pub(crate) fn build_envelope<J: Job>(
    job: &J,
    available_at: chrono::DateTime<chrono::Utc>,
) -> Result<Envelope, FrameworkError> {
    let payload = serde_json::to_value(job)
        .map_err(|e| FrameworkError::internal(format!("encode job: {e}")))?;
    let timeout_secs = J::timeout().map(|d| d.as_secs());
    Ok(Envelope {
        schema_version: CURRENT_SCHEMA_VERSION,
        id: Uuid::new_v4(),
        job_name: J::job_name().to_string(),
        payload,
        dispatched_at: Utc::now(),
        available_at,
        attempts: 0,
        max_tries: J::max_tries(),
        backoff: J::backoff(),
        timeout_secs,
        fail_on_timeout: J::fail_on_timeout(),
        idempotency_key: None,
        batch_id: None,
        chain_remaining: Vec::new(),
    })
}
