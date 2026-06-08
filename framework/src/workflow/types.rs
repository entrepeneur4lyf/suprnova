//! Workflow public types

use crate::error::FrameworkError;
use crate::workflow::WorkflowConfig;
use crate::workflow::store;
use serde::de::DeserializeOwned;
use std::time::Duration;

/// Workflow execution status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowStatus {
    /// Workflow is queued but not yet claimed by a worker.
    Pending,
    /// A worker holds the lease and is currently executing the workflow.
    Running,
    /// Workflow finished successfully and persisted its output.
    Succeeded,
    /// Workflow exhausted its attempts and is recorded as failed.
    Failed,
}

impl WorkflowStatus {
    /// Database string representation (`"pending"` / `"running"` / `"succeeded"` / `"failed"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    /// Parse from the database string representation; returns `None` for unknown values.
    // Returns Option rather than Result, so implementing FromStr would change semantics.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Step execution status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// Step is currently executing on a worker.
    Running,
    /// Step finished successfully and persisted its output.
    Succeeded,
    /// Step exhausted its attempts and is recorded as failed.
    Failed,
}

impl StepStatus {
    /// Database string representation (`"running"` / `"succeeded"` / `"failed"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

    /// Parse from the database string representation; returns `None` for unknown values.
    // Returns Option rather than Result, so implementing FromStr would change semantics.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Handle for a workflow instance
#[derive(Debug, Clone)]
pub struct WorkflowHandle {
    id: i64,
}

impl WorkflowHandle {
    pub(crate) fn new(id: i64) -> Self {
        Self { id }
    }

    /// Workflow id
    pub fn id(&self) -> i64 {
        self.id
    }

    /// Fetch the current status
    pub async fn status(&self) -> Result<WorkflowStatus, FrameworkError> {
        store::get_workflow_status(self.id).await
    }

    /// Wait until the workflow finishes (succeeded/failed). Polls
    /// indefinitely. Prefer [`Self::wait_with_timeout`] for callers that
    /// cannot afford to hang on a stuck or lost workflow.
    pub async fn wait(&self) -> Result<WorkflowStatus, FrameworkError> {
        let config = WorkflowConfig::default();
        let poll = Duration::from_millis(config.poll_interval_ms);
        self.wait_inner(poll, None).await
    }

    /// Wait until the workflow finishes or `timeout` elapses, whichever
    /// comes first. Returns `Err(FrameworkError::Timeout(...))` when the
    /// deadline fires while the workflow is still pending or running.
    ///
    /// A timeout error does **not** cancel the workflow — the worker
    /// continues processing it. Re-call `wait*` later, or use
    /// [`Self::status`] to poll directly.
    pub async fn wait_with_timeout(
        &self,
        timeout: Duration,
    ) -> Result<WorkflowStatus, FrameworkError> {
        let config = WorkflowConfig::default();
        let poll = Duration::from_millis(config.poll_interval_ms);
        self.wait_inner(poll, Some(timeout)).await
    }

    /// Wait with full control over the polling interval and timeout.
    /// `poll_interval` defaults to [`WorkflowConfig::poll_interval_ms`]
    /// when `None`. A `None` `timeout` polls indefinitely.
    pub async fn wait_with_options(
        &self,
        poll_interval: Option<Duration>,
        timeout: Option<Duration>,
    ) -> Result<WorkflowStatus, FrameworkError> {
        let poll = poll_interval.unwrap_or_else(|| {
            let config = WorkflowConfig::default();
            Duration::from_millis(config.poll_interval_ms)
        });
        self.wait_inner(poll, timeout).await
    }

    async fn wait_inner(
        &self,
        poll: Duration,
        timeout: Option<Duration>,
    ) -> Result<WorkflowStatus, FrameworkError> {
        // tokio::time::timeout wraps the whole poll loop so the caller's
        // deadline always wins, even if the underlying `status()` query
        // hangs on a database stall.
        let workflow_id = self.id;
        let fut = async move {
            loop {
                let status = self.status().await?;
                match status {
                    WorkflowStatus::Succeeded | WorkflowStatus::Failed => return Ok(status),
                    _ => tokio::time::sleep(poll).await,
                }
            }
        };

        match timeout {
            Some(deadline) => match tokio::time::timeout(deadline, fut).await {
                Ok(result) => result,
                Err(_) => Err(FrameworkError::internal(format!(
                    "Timed out after {:?} waiting for workflow {workflow_id} to finish",
                    deadline
                ))),
            },
            None => fut.await,
        }
    }

    /// Get the raw output JSON (if any)
    pub async fn output_raw(&self) -> Result<Option<String>, FrameworkError> {
        store::get_workflow_output(self.id).await
    }

    /// Deserialize output JSON into a type
    pub async fn output<T: DeserializeOwned>(&self) -> Result<Option<T>, FrameworkError> {
        match self.output_raw().await? {
            Some(json) => {
                let value = serde_json::from_str(&json).map_err(|e| {
                    FrameworkError::internal(format!("Workflow output deserialize error: {}", e))
                })?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }
}

/// Workflow record used by the worker
#[derive(Debug, Clone)]
pub struct ClaimedWorkflow {
    /// Workflow row primary key.
    pub id: i64,
    /// Registered workflow name (the `#[workflow]` ident).
    pub name: String,
    /// Serialised workflow input (JSON).
    pub input: String,
    /// Number of times this workflow has been attempted so far.
    pub attempts: i32,
    /// Maximum number of attempts before the workflow is marked failed.
    pub max_attempts: i32,
}
