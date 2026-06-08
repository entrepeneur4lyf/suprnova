//! Synchronous queue driver — runs jobs inline on `push`.
//!
//! Mirrors Laravel's `SyncQueue`. The envelope is dispatched via the worker
//! registry (`dispatch_by_name`) before `push` returns; there is no
//! background worker, no retry, and no delayed-job support — `push` for an
//! envelope with `available_at` in the future runs immediately anyway, just
//! like Laravel's sync driver (a "fake" queue for development).
//!
//! Use this driver in stages where the queue infra isn't desired (CI without
//! Redis, local dev), or in tests that need handlers to actually execute
//! without the worker loop. Production deployments should use Memory (for
//! single-process apps), Redis, or Database.

use crate::error::FrameworkError;
use crate::queue::driver::{QueueDriver, Reservation, ReservationToken};
use crate::queue::envelope::Envelope;
use crate::queue::worker::dispatch_by_name;
use async_trait::async_trait;
use std::time::Duration;

/// [`QueueDriver`] that runs each pushed job inline on the calling
/// task. Mirrors Laravel's `sync` driver — useful in tests and in
/// configurations where no background worker is desired.
#[derive(Default)]
pub struct SyncQueueDriver;

impl SyncQueueDriver {
    /// Construct a fresh sync driver.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl QueueDriver for SyncQueueDriver {
    async fn push(&self, env: Envelope) -> Result<(), FrameworkError> {
        // Sync drivers run the dispatcher inline. If no dispatcher is
        // registered for this job_name, `dispatch_by_name` returns the
        // same "unknown job" error a worker would. Errors propagate to
        // the caller of `Queue::push` — there is no retry path because
        // there is no background worker.
        dispatch_by_name(&env.job_name, env.payload).await
    }

    async fn pop(&self, _vt: Duration) -> Result<Option<Reservation>, FrameworkError> {
        // Sync driver has no queue to pop from; the worker should never
        // be running against it.
        Ok(None)
    }

    async fn ack(&self, _t: &ReservationToken) -> Result<(), FrameworkError> {
        Ok(())
    }

    async fn nack(&self, _t: &ReservationToken, _delay: Duration) -> Result<(), FrameworkError> {
        Ok(())
    }

    async fn size(&self) -> Result<u64, FrameworkError> {
        Ok(0)
    }

    async fn clear(&self) -> Result<u64, FrameworkError> {
        Ok(0)
    }

    fn name(&self) -> &'static str {
        "sync"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Job;
    use crate::queue::worker::register_job;
    use crate::queue::{CURRENT_SCHEMA_VERSION, Envelope};
    use async_trait::async_trait;
    use chrono::Utc;
    use serde::{Deserialize, Serialize};
    use serial_test::serial;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use uuid::Uuid;

    static SYNC_RUNS: AtomicU32 = AtomicU32::new(0);

    #[derive(Serialize, Deserialize)]
    struct SyncJob {
        x: i32,
    }

    #[async_trait]
    impl Job for SyncJob {
        fn job_name() -> &'static str {
            "queue::sync::tests::SyncJob"
        }
        async fn handle(self) -> Result<(), FrameworkError> {
            SYNC_RUNS.fetch_add(self.x as u32, Ordering::SeqCst);
            Ok(())
        }
    }

    fn env_for<J: Job>(job: &J) -> Envelope {
        Envelope {
            schema_version: CURRENT_SCHEMA_VERSION,
            id: Uuid::new_v4(),
            job_name: J::job_name().into(),
            payload: serde_json::to_value(job).unwrap(),
            dispatched_at: Utc::now(),
            available_at: Utc::now(),
            attempts: 0,
            max_tries: 1,
            backoff: crate::queue::BackoffSchedule::default(),
            timeout_secs: None,
            fail_on_timeout: false,
            idempotency_key: None,
            batch_id: None,
            chain_remaining: Vec::new(),
        }
    }

    #[tokio::test]
    #[serial]
    async fn sync_driver_runs_inline() {
        SYNC_RUNS.store(0, Ordering::SeqCst);
        register_job::<SyncJob>();
        let d = Arc::new(SyncQueueDriver::new());
        d.push(env_for(&SyncJob { x: 7 })).await.unwrap();
        assert_eq!(SYNC_RUNS.load(Ordering::SeqCst), 7);
    }
}
