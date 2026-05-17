//! Job envelope (BREAKING-CHANGE-FROZEN v1).
//!
//! Every queue driver round-trips through this exact JSON layout.
//! Bumping `schema_version` requires a dual-read worker for one minor release.

use crate::queue::job::BackoffSchedule;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

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
}

#[derive(Debug, thiserror::Error)]
pub enum EnvelopeError {
    #[error("unsupported queue envelope schema_version: {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("envelope decode error: {0}")]
    Decode(#[from] serde_json::Error),
}

impl Envelope {
    pub fn from_json(s: &str) -> Result<Self, EnvelopeError> {
        let env: Envelope = serde_json::from_str(s)?;
        if env.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(EnvelopeError::UnsupportedSchemaVersion(env.schema_version));
        }
        Ok(env)
    }

    pub fn to_json(&self) -> Result<String, EnvelopeError> {
        Ok(serde_json::to_string(self)?)
    }
}
