//! Workflow persistence helpers

use crate::database::DB;
use crate::error::FrameworkError;
use crate::workflow::config::WorkflowConfig;
use crate::workflow::entities::{workflow_steps, workflows};
use crate::workflow::types::{ClaimedWorkflow, StepStatus, WorkflowHandle, WorkflowStatus};
use chrono::{Duration as ChronoDuration, Utc};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseBackend, EntityTrait, QueryFilter, Set};
use sea_orm::{ConnectionTrait, Statement};
use std::time::Duration;

/// Insert a new workflow row (pending)
pub async fn insert_workflow(
    name: &str,
    input: &str,
    max_attempts: i32,
) -> Result<WorkflowHandle, FrameworkError> {
    let db = DB::connection()?;
    let now = Utc::now().naive_utc();

    let model = workflows::ActiveModel {
        name: Set(name.to_string()),
        status: Set(WorkflowStatus::Pending.as_str().to_string()),
        input: Set(input.to_string()),
        output: Set(None),
        error: Set(None),
        attempts: Set(0),
        max_attempts: Set(max_attempts),
        next_run_at: Set(None),
        locked_until: Set(None),
        worker_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        started_at: Set(None),
        completed_at: Set(None),
        ..Default::default()
    };

    let inserted = model
        .insert(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;

    Ok(WorkflowHandle::new(inserted.id))
}

/// Get workflow status
pub async fn get_workflow_status(id: i64) -> Result<WorkflowStatus, FrameworkError> {
    let db = DB::connection()?;
    let model = workflows::Entity::find_by_id(id)
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?
        .ok_or_else(|| FrameworkError::internal("Workflow not found"))?;

    WorkflowStatus::from_str(&model.status)
        .ok_or_else(|| FrameworkError::internal("Invalid workflow status"))
}

/// Get workflow output JSON
pub async fn get_workflow_output(id: i64) -> Result<Option<String>, FrameworkError> {
    let db = DB::connection()?;
    let model = workflows::Entity::find_by_id(id)
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?
        .ok_or_else(|| FrameworkError::internal("Workflow not found"))?;
    Ok(model.output)
}

/// Load workflow record by id
pub async fn get_workflow_record(id: i64) -> Result<workflows::Model, FrameworkError> {
    let db = DB::connection()?;
    workflows::Entity::find_by_id(id)
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?
        .ok_or_else(|| FrameworkError::internal("Workflow not found"))
}

/// Mark workflow as running (used for tests or manual claim)
pub async fn mark_running(
    id: i64,
    worker_id: &str,
    lock_timeout: Duration,
) -> Result<ClaimedWorkflow, FrameworkError> {
    let db = DB::connection()?;
    let now = Utc::now().naive_utc();
    let lock_until = now + ChronoDuration::seconds(lock_timeout.as_secs() as i64);

    let model = workflows::Entity::find_by_id(id)
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?
        .ok_or_else(|| FrameworkError::internal("Workflow not found"))?;

    let attempts = model.attempts + 1;
    let started_at = model.started_at.unwrap_or(now);
    let mut active: workflows::ActiveModel = model.into();
    active.attempts = Set(attempts);
    active.status = Set(WorkflowStatus::Running.as_str().to_string());
    active.locked_until = Set(Some(lock_until));
    active.worker_id = Set(Some(worker_id.to_string()));
    active.started_at = Set(Some(started_at));
    active.updated_at = Set(now);

    let updated = active
        .update(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;

    Ok(ClaimedWorkflow {
        id: updated.id,
        name: updated.name,
        input: updated.input,
        attempts: updated.attempts,
        max_attempts: updated.max_attempts,
    })
}

/// Claim the next workflow to run (Postgres only)
pub async fn claim_next_workflow(
    worker_id: &str,
    config: &WorkflowConfig,
) -> Result<Option<ClaimedWorkflow>, FrameworkError> {
    let db = DB::connection()?;
    let backend = db.inner().get_database_backend();
    if backend != DatabaseBackend::Postgres {
        return Err(FrameworkError::internal(
            "Workflow worker requires a Postgres database",
        ));
    }

    let lock_until =
        Utc::now().naive_utc() + ChronoDuration::seconds(config.lock_timeout_secs as i64);

    let sql = r#"
        UPDATE workflows
        SET status = 'running',
            attempts = attempts + 1,
            locked_until = $1,
            worker_id = $2,
            started_at = COALESCE(started_at, NOW()),
            updated_at = NOW()
        WHERE id = (
            SELECT id
            FROM workflows
            WHERE status = 'pending'
              AND (next_run_at IS NULL OR next_run_at <= NOW())
              AND (locked_until IS NULL OR locked_until <= NOW())
            ORDER BY id
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        RETURNING id, name, input, attempts, max_attempts
    "#;

    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        vec![lock_until.into(), worker_id.into()],
    );

    let row = db
        .inner()
        .query_one(stmt)
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;

    if let Some(row) = row {
        let id: i64 = row
            .try_get("", "id")
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let name: String = row
            .try_get("", "name")
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let input: String = row
            .try_get("", "input")
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let attempts: i32 = row
            .try_get("", "attempts")
            .map_err(|e| FrameworkError::database(e.to_string()))?;
        let max_attempts: i32 = row
            .try_get("", "max_attempts")
            .map_err(|e| FrameworkError::database(e.to_string()))?;

        Ok(Some(ClaimedWorkflow {
            id,
            name,
            input,
            attempts,
            max_attempts,
        }))
    } else {
        Ok(None)
    }
}

/// Refresh workflow lock lease
pub async fn refresh_lock(id: i64, lock_timeout: Duration) -> Result<(), FrameworkError> {
    let db = DB::connection()?;
    let now = Utc::now().naive_utc();
    let lock_until = now + ChronoDuration::seconds(lock_timeout.as_secs() as i64);

    let mut active: workflows::ActiveModel = workflows::Entity::find_by_id(id)
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?
        .ok_or_else(|| FrameworkError::internal("Workflow not found"))?
        .into();

    active.locked_until = Set(Some(lock_until));
    active.updated_at = Set(now);

    active
        .update(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;

    Ok(())
}

/// Mark workflow as succeeded
pub async fn mark_succeeded(id: i64, output: &str) -> Result<(), FrameworkError> {
    let db = DB::connection()?;
    let now = Utc::now().naive_utc();

    let mut active: workflows::ActiveModel = workflows::Entity::find_by_id(id)
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?
        .ok_or_else(|| FrameworkError::internal("Workflow not found"))?
        .into();

    active.status = Set(WorkflowStatus::Succeeded.as_str().to_string());
    active.output = Set(Some(output.to_string()));
    active.error = Set(None);
    active.completed_at = Set(Some(now));
    active.locked_until = Set(None);
    active.worker_id = Set(None);
    active.updated_at = Set(now);

    active
        .update(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;

    Ok(())
}

/// Requeue workflow for retry
pub async fn requeue(
    id: i64,
    error: &str,
    next_run_at: chrono::NaiveDateTime,
) -> Result<(), FrameworkError> {
    let db = DB::connection()?;
    let now = Utc::now().naive_utc();

    let mut active: workflows::ActiveModel = workflows::Entity::find_by_id(id)
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?
        .ok_or_else(|| FrameworkError::internal("Workflow not found"))?
        .into();

    active.status = Set(WorkflowStatus::Pending.as_str().to_string());
    active.error = Set(Some(error.to_string()));
    active.next_run_at = Set(Some(next_run_at));
    active.locked_until = Set(None);
    active.worker_id = Set(None);
    active.updated_at = Set(now);

    active
        .update(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;

    Ok(())
}

/// Mark workflow as failed
pub async fn mark_failed(id: i64, error: &str) -> Result<(), FrameworkError> {
    let db = DB::connection()?;
    let now = Utc::now().naive_utc();

    let mut active: workflows::ActiveModel = workflows::Entity::find_by_id(id)
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?
        .ok_or_else(|| FrameworkError::internal("Workflow not found"))?
        .into();

    active.status = Set(WorkflowStatus::Failed.as_str().to_string());
    active.error = Set(Some(error.to_string()));
    active.completed_at = Set(Some(now));
    active.locked_until = Set(None);
    active.worker_id = Set(None);
    active.updated_at = Set(now);

    active
        .update(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;

    Ok(())
}

/// Load a step by workflow + index
pub async fn load_step(
    workflow_id: i64,
    step_index: i32,
    step_name: &str,
) -> Result<Option<workflow_steps::Model>, FrameworkError> {
    let db = DB::connection()?;
    workflow_steps::Entity::find()
        .filter(workflow_steps::Column::WorkflowId.eq(workflow_id))
        .filter(workflow_steps::Column::StepIndex.eq(step_index))
        .filter(workflow_steps::Column::StepName.eq(step_name))
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))
}

/// Load any step by workflow + index (used to detect mismatches)
pub async fn load_step_by_index(
    workflow_id: i64,
    step_index: i32,
) -> Result<Option<workflow_steps::Model>, FrameworkError> {
    let db = DB::connection()?;
    workflow_steps::Entity::find()
        .filter(workflow_steps::Column::WorkflowId.eq(workflow_id))
        .filter(workflow_steps::Column::StepIndex.eq(step_index))
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))
}

/// Insert a running step
pub async fn insert_step_running(
    workflow_id: i64,
    step_index: i32,
    step_name: &str,
    input: &str,
) -> Result<(), FrameworkError> {
    let db = DB::connection()?;
    let now = Utc::now().naive_utc();

    let model = workflow_steps::ActiveModel {
        workflow_id: Set(workflow_id),
        step_index: Set(step_index),
        step_name: Set(step_name.to_string()),
        status: Set(StepStatus::Running.as_str().to_string()),
        input: Set(input.to_string()),
        output: Set(None),
        error: Set(None),
        attempts: Set(1),
        created_at: Set(now),
        updated_at: Set(now),
        started_at: Set(Some(now)),
        completed_at: Set(None),
        ..Default::default()
    };

    model
        .insert(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;

    Ok(())
}

/// Update a step to running and increment attempts
pub async fn update_step_running(
    step: workflow_steps::Model,
    input: &str,
) -> Result<(), FrameworkError> {
    let db = DB::connection()?;
    let now = Utc::now().naive_utc();

    let attempts = step.attempts + 1;
    let mut active: workflow_steps::ActiveModel = step.into();
    active.status = Set(StepStatus::Running.as_str().to_string());
    active.input = Set(input.to_string());
    active.attempts = Set(attempts);
    active.updated_at = Set(now);
    active.started_at = Set(Some(now));

    active
        .update(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;

    Ok(())
}

/// Mark step succeeded
pub async fn mark_step_succeeded(step_id: i64, output: &str) -> Result<(), FrameworkError> {
    let db = DB::connection()?;
    let now = Utc::now().naive_utc();

    let mut active: workflow_steps::ActiveModel = workflow_steps::Entity::find_by_id(step_id)
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?
        .ok_or_else(|| FrameworkError::internal("Step not found"))?
        .into();

    active.status = Set(StepStatus::Succeeded.as_str().to_string());
    active.output = Set(Some(output.to_string()));
    active.error = Set(None);
    active.updated_at = Set(now);
    active.completed_at = Set(Some(now));

    active
        .update(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;

    Ok(())
}

/// Mark step failed
pub async fn mark_step_failed(step_id: i64, error: &str) -> Result<(), FrameworkError> {
    let db = DB::connection()?;
    let now = Utc::now().naive_utc();

    let mut active: workflow_steps::ActiveModel = workflow_steps::Entity::find_by_id(step_id)
        .one(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?
        .ok_or_else(|| FrameworkError::internal("Step not found"))?
        .into();

    active.status = Set(StepStatus::Failed.as_str().to_string());
    active.error = Set(Some(error.to_string()));
    active.updated_at = Set(now);
    active.completed_at = Set(Some(now));

    active
        .update(db.inner())
        .await
        .map_err(|e| FrameworkError::database(e.to_string()))?;

    Ok(())
}
