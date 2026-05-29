//! Job envelope (BREAKING-CHANGE-FROZEN v1).
//!
//! Every queue driver round-trips through this exact JSON layout.
//! Bumping `schema_version` requires a dual-read worker for one minor release.

use crate::queue::chain::ChainLink;
use crate::queue::job::BackoffSchedule;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CURRENT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub schema_version: u32,
    pub id: Uuid,
    pub job_name: String,
    pub payload: serde_json::Value,
    pub dispatched_at: DateTime<Utc>,
    pub available_at: DateTime<Utc>,
    pub attempts: u32,
    pub max_tries: u32,
    pub backoff: BackoffSchedule,
    pub timeout_secs: Option<u64>,
    pub fail_on_timeout: bool,
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

#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    #[error("unsupported queue envelope schema_version: {0}")]
    UnsupportedSchemaVersion(u32),
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

    pub fn to_json(&self) -> Result<String, EnvelopeError> {
        Ok(serde_json::to_string(self)?)
    }
}
