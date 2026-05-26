//! Workflow public types

use crate::error::FrameworkError;
use crate::workflow::WorkflowConfig;
use crate::workflow::store;
use serde::de::DeserializeOwned;
use std::time::Duration;

/// Workflow execution status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl WorkflowStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

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
    Running,
    Succeeded,
    Failed,
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }

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

    /// Wait until the workflow finishes (succeeded/failed)
    pub async fn wait(&self) -> Result<WorkflowStatus, FrameworkError> {
        let config = WorkflowConfig::default();
        let poll = Duration::from_millis(config.poll_interval_ms);

        loop {
            let status = self.status().await?;
            match status {
                WorkflowStatus::Succeeded | WorkflowStatus::Failed => return Ok(status),
                _ => tokio::time::sleep(poll).await,
            }
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
    pub id: i64,
    pub name: String,
    pub input: String,
    pub attempts: i32,
    pub max_attempts: i32,
}
