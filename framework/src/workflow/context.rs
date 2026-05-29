//! Workflow execution context

use crate::error::FrameworkError;
use crate::workflow::store;
use crate::workflow::types::StepStatus;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

#[derive(Clone)]
pub struct WorkflowContext {
    inner: Arc<WorkflowContextInner>,
}

struct WorkflowContextInner {
    workflow_id: i64,
    lock_timeout: Duration,
    step_index: AtomicI32,
}

tokio::task_local! {
    static CONTEXT: WorkflowContext;
}

impl WorkflowContext {
    pub(crate) fn new(workflow_id: i64, lock_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(WorkflowContextInner {
                workflow_id,
                lock_timeout,
                step_index: AtomicI32::new(0),
            }),
        }
    }

    /// Run a future within this workflow context
    pub async fn enter<T, Fut>(self, fut: Fut) -> T
    where
        Fut: Future<Output = T>,
    {
        CONTEXT.scope(self, fut).await
    }

    /// Get the current workflow context if set
    pub fn current() -> Option<Self> {
        CONTEXT.try_with(|ctx| ctx.clone()).ok()
    }

    /// Check if workflow context is active
    pub fn is_active() -> bool {
        CONTEXT.try_with(|_| ()).is_ok()
    }

    /// Run a workflow step with pre-serialized input JSON
    pub async fn run_step_with_input<F, Fut, T>(
        &self,
        step_name: &str,
        input_json: String,
        f: F,
    ) -> Result<T, FrameworkError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, FrameworkError>> + Send + 'static,
        T: Serialize + DeserializeOwned + Send + 'static,
    {
        let workflow_id = self.inner.workflow_id;
        let step_index = self.inner.step_index.fetch_add(1, Ordering::SeqCst);

        if let Some(existing) = store::load_step(workflow_id, step_index, step_name).await? {
            // Workflows must be deterministic. If the same step at the same
            // index is replayed with different serialized input, the recorded
            // output (if any) belongs to a different invocation and reusing
            // it would silently corrupt downstream steps. Fail loud rather
            // than masking the contract violation by either returning the
            // wrong cached output (Succeeded branch) or quietly overwriting
            // the input column (Running branch).
            if existing.input != input_json {
                return Err(FrameworkError::internal(format!(
                    "Workflow step input mismatch at index {} ('{}'): cached input does not match replay input. \
                     Workflow steps must be deterministic.",
                    step_index, step_name
                )));
            }

            if let Some(status) = StepStatus::from_str(&existing.status)
                && status == StepStatus::Succeeded
            {
                let output_json = existing.output.ok_or_else(|| {
                    FrameworkError::internal("Step output missing for succeeded step")
                })?;
                let value = serde_json::from_str(&output_json).map_err(|e| {
                    FrameworkError::internal(format!(
                        "Workflow step output deserialize error: {}",
                        e
                    ))
                })?;
                store::refresh_lock(workflow_id, self.inner.lock_timeout).await?;
                return Ok(value);
            }

            store::update_step_running(existing, &input_json).await?;
        } else {
            if let Some(other) = store::load_step_by_index(workflow_id, step_index).await?
                && other.step_name != step_name
            {
                return Err(FrameworkError::internal(format!(
                    "Workflow step mismatch at index {}: expected '{}', found '{}'. \
                         Workflow steps must be deterministic.",
                    step_index, step_name, other.step_name
                )));
            }
            store::insert_step_running(workflow_id, step_index, step_name, &input_json).await?;
        }

        store::refresh_lock(workflow_id, self.inner.lock_timeout).await?;

        let result = f().await;

        match result {
            Ok(value) => {
                let output_json = serde_json::to_string(&value).map_err(|e| {
                    FrameworkError::internal(format!("Workflow step output serialize error: {}", e))
                })?;
                if let Some(step) = store::load_step(workflow_id, step_index, step_name).await? {
                    store::mark_step_succeeded(step.id, &output_json).await?;
                }
                store::refresh_lock(workflow_id, self.inner.lock_timeout).await?;
                Ok(value)
            }
            Err(err) => {
                if let Some(step) = store::load_step(workflow_id, step_index, step_name).await? {
                    store::mark_step_failed(step.id, &err.to_string()).await?;
                }
                store::refresh_lock(workflow_id, self.inner.lock_timeout).await?;
                Err(err)
            }
        }
    }

    /// Run a workflow step with serializable arguments
    pub async fn run_step<Args, F, Fut, T>(
        &self,
        step_name: &str,
        args: &Args,
        f: F,
    ) -> Result<T, FrameworkError>
    where
        Args: Serialize,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, FrameworkError>> + Send + 'static,
        T: Serialize + DeserializeOwned + Send + 'static,
    {
        let input_json = serde_json::to_string(args).map_err(|e| {
            FrameworkError::internal(format!("Workflow step input serialize error: {}", e))
        })?;

        self.run_step_with_input(step_name, input_json, f).await
    }
}
