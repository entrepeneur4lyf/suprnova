//! Queue subsystem: facade, drivers, envelope, worker.

pub mod envelope;
pub mod job;
pub mod testing;
pub(crate) mod driver;

pub use envelope::{Envelope, EnvelopeError, CURRENT_SCHEMA_VERSION};
pub use job::{BackoffSchedule, Job};

use crate::error::FrameworkError;
use chrono::Utc;
use driver::QueueDriver;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

static DRIVER: RwLock<Option<Arc<dyn QueueDriver>>> = RwLock::new(None);

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
        if testing::is_active() {
            return testing::record::<J>(&job);
        }
        let env = envelope_for::<J>(&job, Utc::now())?;
        let drv = current_driver()?;
        drv.push(env).await
    }

    /// Push a typed job available at `available_at`. Driver is responsible
    /// for honoring the timestamp.
    pub async fn push_later<J: Job>(
        job: J,
        available_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), FrameworkError> {
        if testing::is_active() {
            return testing::record::<J>(&job);
        }
        let env = envelope_for::<J>(&job, available_at)?;
        let drv = current_driver()?;
        drv.push(env).await
    }

    /// Convenience: push with a delay from `now`.
    pub async fn later<J: Job>(
        delay: std::time::Duration,
        job: J,
    ) -> Result<(), FrameworkError> {
        let available_at = Utc::now()
            + chrono::Duration::from_std(delay)
                .map_err(|e| FrameworkError::internal(format!("delay overflow: {e}")))?;
        Self::push_later(job, available_at).await
    }

    /// Replace the registered driver. Primarily for boot-time wiring;
    /// in tests prefer `testing::install_fake()`.
    pub fn set_driver(driver: Arc<dyn QueueDriver>) {
        *DRIVER.write().expect("queue driver lock poisoned") = Some(driver);
    }
}

pub(crate) fn current_driver() -> Result<Arc<dyn QueueDriver>, FrameworkError> {
    DRIVER
        .read()
        .expect("queue driver lock poisoned")
        .clone()
        .ok_or_else(|| {
            FrameworkError::internal(
                "queue driver not initialized; call Queue::set_driver(...) or install a test fake",
            )
        })
}

fn envelope_for<J: Job>(
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
    })
}
