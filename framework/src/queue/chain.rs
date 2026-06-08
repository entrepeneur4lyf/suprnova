//! Queued chains: dispatch a sequence of jobs where each runs only after
//! the previous one ack's.
//!
//! Mirrors Laravel 13's `Bus::chain([...])`. The first envelope is pushed
//! to the queue with the rest serialized inside a `chain_remaining` field;
//! after each successful settlement the worker pops the next entry and
//! dispatches it.
//!
//! Internally chained envelopes use the queue's normal driver; no special
//! storage layer is required because the chain state travels with the
//! current envelope payload.

use crate::error::FrameworkError;
use crate::queue::Job;
use crate::queue::envelope::Envelope;
use crate::queue::job::BackoffSchedule;
use serde::{Deserialize, Serialize};

/// Serialized form of one chained envelope, persisted on the active
/// envelope's `chain_remaining` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainLink {
    /// Fully-qualified job type name (matches `Job::job_name()`).
    pub job_name: String,
    /// Serialized job payload, captured at chain-build time.
    pub payload: serde_json::Value,
    /// Maximum dispatch attempts for this link.
    pub max_tries: u32,
    /// Per-attempt timeout budget in seconds; `None` disables.
    pub timeout_secs: Option<u64>,
    /// When `true`, a timeout consumes the attempt as a permanent failure.
    pub fail_on_timeout: bool,
    /// Job-side backoff schedule captured at chain-build time. `#[serde(default)]`
    /// keeps schema-v2 chain payloads (which omitted this field) decoding —
    /// they get the framework default just as they did before.
    #[serde(default)]
    pub backoff: BackoffSchedule,
}

impl ChainLink {
    /// Build a chain-link entry from a typed `Job`. Uses the job's
    /// `J::max_tries()` / `J::timeout()` / `J::backoff()` defaults exactly
    /// the way `Queue::push` would.
    pub fn from_job<J: Job>(job: J) -> Result<Self, FrameworkError> {
        Ok(Self {
            job_name: J::job_name().to_string(),
            payload: serde_json::to_value(&job)
                .map_err(|e| FrameworkError::internal(format!("encode chain link: {e}")))?,
            max_tries: J::max_tries(),
            timeout_secs: J::timeout().map(|d| d.as_secs()),
            fail_on_timeout: J::fail_on_timeout(),
            backoff: J::backoff(),
        })
    }

    /// Reify into a dispatchable envelope.
    pub fn to_envelope(&self) -> Envelope {
        let now = chrono::Utc::now();
        Envelope {
            schema_version: crate::queue::CURRENT_SCHEMA_VERSION,
            id: uuid::Uuid::new_v4(),
            job_name: self.job_name.clone(),
            payload: self.payload.clone(),
            dispatched_at: now,
            available_at: now,
            attempts: 0,
            max_tries: self.max_tries,
            backoff: self.backoff.clone(),
            timeout_secs: self.timeout_secs,
            fail_on_timeout: self.fail_on_timeout,
            idempotency_key: None,
            batch_id: None,
            chain_remaining: Vec::new(),
        }
    }
}

/// Builder used by [`Queue::chain`](crate::queue::Queue::chain). Mirrors
/// Laravel's `Bus::chain([...])->dispatch()`.
pub struct PendingChain {
    links: Vec<ChainLink>,
}

impl Default for PendingChain {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingChain {
    /// Construct an empty pending chain with no links.
    pub fn new() -> Self {
        Self { links: Vec::new() }
    }

    /// Append a typed job to the chain.
    #[allow(clippy::should_implement_trait)]
    pub fn add<J: Job>(mut self, job: J) -> Result<Self, FrameworkError> {
        self.links.push(ChainLink::from_job(job)?);
        Ok(self)
    }

    /// Number of links queued so far.
    pub fn len(&self) -> usize {
        self.links.len()
    }
    /// `true` when the chain has no links queued yet.
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Dispatch the chain. The first link is pushed immediately; the rest
    /// travel on its `chain_remaining` payload field.
    pub async fn dispatch(self) -> Result<(), FrameworkError> {
        if self.links.is_empty() {
            return Ok(());
        }
        let driver = crate::queue::current_driver()?;
        let mut iter = self.links.into_iter();
        let head = iter.next().unwrap();
        let tail: Vec<ChainLink> = iter.collect();
        let mut env = head.to_envelope();
        env.chain_remaining = tail;
        driver.push(env).await
    }
}
