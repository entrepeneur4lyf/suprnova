//! Job envelope (BREAKING-CHANGE-FROZEN v1).
//!
//! Every queue driver round-trips through this exact JSON layout.
//! Bumping `schema_version` requires a dual-read worker for one minor release.

use crate::queue::chain::ChainLink;
use crate::queue::job::BackoffSchedule;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Highest [`Envelope::schema_version`] this build knows how to read.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Wire-format envelope every queue driver round-trips on push and pop.
///
/// Bumping fields requires a `schema_version` increment and a dual-read
/// worker for one minor release — see the module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Envelope schema version; rejected on pop if greater than
    /// [`CURRENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Unique envelope identifier assigned at push time.
    pub id: Uuid,
    /// Fully-qualified job type name (matches `Job::name()`).
    pub job_name: String,
    /// Typed handler payload as JSON.
    pub payload: serde_json::Value,
    /// When the envelope was first pushed.
    pub dispatched_at: DateTime<Utc>,
    /// Earliest moment a worker may claim this envelope.
    pub available_at: DateTime<Utc>,
    /// Number of attempts already dispatched (incremented on each pop).
    pub attempts: u32,
    /// Maximum attempts before the worker dead-letters the job.
    pub max_tries: u32,
    /// Backoff schedule consulted when a failed attempt is re-released.
    pub backoff: BackoffSchedule,
    /// Per-attempt timeout budget, in seconds; `None` disables the timeout.
    pub timeout_secs: Option<u64>,
    /// When `true`, a timeout consumes the attempt as a permanent failure.
    pub fail_on_timeout: bool,
    /// Dedupe id stamped by the [`Queue::push_unique`](crate::queue::Queue::push_unique)
    /// family at push time and recorded on the envelope for observability.
    /// Push-time uniqueness is enforced via
    /// [`Idempotency::commit_on_success`](crate::idempotency::Idempotency::commit_on_success)
    /// keyed on this id; the worker does **not** consult this field on
    /// redelivery. At-least-once delivery means handlers must still be
    /// idempotent on their own (see the
    /// [worker module docs](crate::queue::worker) for the recommended
    /// `Idempotency::once` / `commit_on_success` / `remember` patterns).
    /// Cleared by [`Queue::retry_failed`](crate::queue::Queue::retry_failed)
    /// and `retry_all_failed` so a retried envelope re-enters the queue
    /// without occupying the unique slot of the original dispatch.
    pub idempotency_key: Option<String>,
    /// Owning batch id when this envelope was dispatched as part of a
    /// [`PendingBatch`](crate::queue::batch::PendingBatch). `None` for
    /// non-batched jobs.
    #[serde(default)]
    pub batch_id: Option<String>,
    /// Tail of a queued chain — remaining links to dispatch after this
    /// envelope's handler reports success. Empty for non-chained jobs
    /// (the common case).
    #[serde(default)]
    pub chain_remaining: Vec<ChainLink>,
}

/// Errors raised when decoding or validating a queue envelope.
#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    /// The envelope's `schema_version` is newer than [`CURRENT_SCHEMA_VERSION`].
    #[error("unsupported queue envelope schema_version: {0}")]
    UnsupportedSchemaVersion(u32),
    /// The envelope JSON failed to parse against the [`Envelope`] schema.
    #[error("envelope decode error: {0}")]
    Decode(#[from] serde_json::Error),
}

impl Envelope {
    /// Decode an envelope, accepting both schema v1 and v2. v1 envelopes
    /// land with empty `batch_id` / `chain_remaining` via serde defaults —
    /// the new fields don't change semantics for jobs that pre-date them.
    pub fn from_json(s: &str) -> Result<Self, EnvelopeError> {
        let env: Envelope = serde_json::from_str(s)?;
        if env.schema_version > CURRENT_SCHEMA_VERSION {
            return Err(EnvelopeError::UnsupportedSchemaVersion(env.schema_version));
        }
        Ok(env)
    }

    /// Serialize the envelope to its canonical JSON wire form.
    pub fn to_json(&self) -> Result<String, EnvelopeError> {
        Ok(serde_json::to_string(self)?)
    }
}
