//! Durable workflow engine
//!
//! Provides a Postgres-backed durable workflow system with step persistence
//! and automatic retries. Inspired by Laravel queues and DBOS.
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::{workflow, workflow_step, start_workflow, FrameworkError};
//!
//! #[workflow_step]
//! async fn fetch_user(user_id: i64) -> Result<String, FrameworkError> {
//!     Ok(format!("user:{}", user_id))
//! }
//!
//! #[workflow_step]
//! async fn send_email(user: String) -> Result<(), FrameworkError> {
//!     println!("Sending email to {}", user);
//!     Ok(())
//! }
//!
//! #[workflow]
//! async fn welcome_flow(user_id: i64) -> Result<(), FrameworkError> {
//!     let user = fetch_user(user_id).await?;
//!     send_email(user).await?;
//!     Ok(())
//! }
//!
//! // Enqueue a workflow
//! // let handle = start_workflow!(welcome_flow, 123).await?;
//! // handle.wait().await?;
//!
//! // Run worker (separate process):
//! // suprnova workflow:work
//! ```

pub mod config;
pub mod context;
pub mod entities;
#[doc(hidden)]
pub mod registry;
pub mod store;
pub mod types;

pub use config::WorkflowConfig;
pub use context::WorkflowContext;
pub use types::{StepStatus, WorkflowHandle, WorkflowStatus};

use crate::config::Config;
use crate::error::FrameworkError;
use crate::workflow::types::ClaimedWorkflow;
use chrono::{Duration as ChronoDuration, Utc};
use rand::RngExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Start a workflow by name with serialized input JSON
pub async fn start_named(name: &str, input: &str) -> Result<WorkflowHandle, FrameworkError> {
    if registry::find(name).is_none() {
        return Err(FrameworkError::internal(format!(
            "Workflow '{}' is not registered",
            name
        )));
    }

    let config = Config::get::<WorkflowConfig>().unwrap_or_default();
    store::insert_workflow(name, input, config.max_attempts).await
}

/// Normalize a workflow name to module_path::fn_name form
pub fn normalize_workflow_name(name: &str) -> String {
    let trimmed = name.replace(' ', "");
    if trimmed.contains("::") {
        trimmed
    } else {
        format!("{}::{}", module_path!(), trimmed)
    }
}

/// Workflow worker daemon
pub struct WorkflowWorker {
    config: Arc<WorkflowConfig>,
    worker_id: String,
}

impl Default for WorkflowWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkflowWorker {
    /// Create a worker with config from environment
    pub fn new() -> Self {
        let config = Config::get::<WorkflowConfig>().unwrap_or_default();
        Self::with_config(config)
    }

    /// Create a worker with a custom config
    pub fn with_config(config: WorkflowConfig) -> Self {
        let random: u64 = rand::rng().random();
        let worker_id = format!("{}-{}", std::process::id(), random);
        Self {
            config: Arc::new(config),
            worker_id,
        }
    }

    /// Run the worker loop indefinitely
    pub async fn work_loop() -> Result<(), FrameworkError> {
        Self::new().run().await
    }

    async fn run(self) -> Result<(), FrameworkError> {
        let poll = Duration::from_millis(self.config.poll_interval_ms);
        let semaphore = Arc::new(Semaphore::new(self.config.concurrency));

        loop {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let claim = store::claim_next_workflow(&self.worker_id, &self.config).await;

            match claim {
                Ok(Some(claimed)) => {
                    let config = self.config.clone();
                    let worker_id = self.worker_id.clone();
                    tokio::spawn(async move {
                        if let Err(err) =
                            process_claimed_workflow(claimed, config, &worker_id).await
                        {
                            eprintln!("Workflow execution error: {}", err);
                        }
                        drop(permit);
                    });
                }
                Ok(None) => {
                    drop(permit);
                    tokio::time::sleep(poll).await;
                }
                Err(err) => {
                    eprintln!("Workflow claim error: {}", err);
                    drop(permit);
                    tokio::time::sleep(poll).await;
                }
            }
        }
    }
}

async fn process_claimed_workflow(
    claimed: ClaimedWorkflow,
    config: Arc<WorkflowConfig>,
    _worker_id: &str,
) -> Result<(), FrameworkError> {
    let entry = match registry::find(&claimed.name) {
        Some(entry) => entry,
        None => {
            store::mark_failed(claimed.id, "Workflow not registered").await?;
            return Ok(());
        }
    };

    let ctx = WorkflowContext::new(claimed.id, Duration::from_secs(config.lock_timeout_secs));

    let result = ctx.enter(async { (entry.run)(&claimed.input).await }).await;

    match result {
        Ok(output) => {
            store::mark_succeeded(claimed.id, &output).await?;
        }
        Err(err) => {
            if claimed.attempts < claimed.max_attempts {
                let backoff = config.retry_backoff_secs * claimed.attempts as i64;
                let next_run_at = Utc::now().naive_utc() + ChronoDuration::seconds(backoff);
                store::requeue(claimed.id, &err.to_string(), next_run_at).await?;
            } else {
                store::mark_failed(claimed.id, &err.to_string()).await?;
            }
        }
    }

    Ok(())
}

/// Enqueue a workflow by function name with serialized args
///
/// Example:
/// ```rust,ignore
/// let handle = start_workflow!(my_workflow, 42, "hello").await?;
/// ```
#[macro_export]
macro_rules! start_workflow {
    ($workflow:path $(, $arg:expr)* $(,)?) => {{
        async {
            let __name = stringify!($workflow);
            let __name = if __name.contains("::") {
                __name.to_string()
            } else {
                format!("{}::{}", module_path!(), __name)
            };
            let __name = __name.replace(' ', "");
            let __input = ::suprnova::serde_json::to_string(&( $($arg,)* ))
                .map_err(|e| ::suprnova::FrameworkError::internal(format!("Workflow input serialize error: {}", e)))?;
            ::suprnova::workflow::start_named(&__name, &__input).await
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestDatabase;
    use sea_orm_migration::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use suprnova_macros::{workflow, workflow_step};

    static ALWAYS_CALLS: AtomicUsize = AtomicUsize::new(0);
    static FLAKY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CACHE_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[workflow_step]
    async fn always_step() -> Result<i32, FrameworkError> {
        ALWAYS_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(1)
    }

    #[workflow_step]
    async fn flaky_step() -> Result<i32, FrameworkError> {
        let attempt = FLAKY_CALLS.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            Err(FrameworkError::internal("flaky"))
        } else {
            Ok(2)
        }
    }

    #[workflow]
    async fn test_workflow() -> Result<i32, FrameworkError> {
        let a = always_step().await?;
        let b = flaky_step().await?;
        Ok(a + b)
    }

    #[workflow]
    async fn name_norm_workflow(value: i32) -> Result<i32, FrameworkError> {
        Ok(value)
    }

    #[tokio::test]
    async fn test_step_caching() {
        let _db = setup_db().await;
        CACHE_CALLS.store(0, Ordering::SeqCst);

        let handle = store::insert_workflow("cache", "{}", 3)
            .await
            .expect("workflow insert");

        let ctx = WorkflowContext::new(handle.id(), Duration::from_secs(30));
        let ctx_inner = ctx.clone();
        let _ = ctx
            .enter(async move {
                ctx_inner
                    .run_step_with_input(
                        "cache-step",
                        serde_json::to_string(&()).unwrap(),
                        || async {
                            CACHE_CALLS.fetch_add(1, Ordering::SeqCst);
                            Ok::<_, FrameworkError>(42)
                        },
                    )
                    .await
                    .unwrap()
            })
            .await;

        let ctx2 = WorkflowContext::new(handle.id(), Duration::from_secs(30));
        let ctx2_inner = ctx2.clone();
        let value = ctx2
            .enter(async move {
                ctx2_inner
                    .run_step_with_input(
                        "cache-step",
                        serde_json::to_string(&()).unwrap(),
                        || async {
                            CACHE_CALLS.fetch_add(1, Ordering::SeqCst);
                            Ok::<_, FrameworkError>(99)
                        },
                    )
                    .await
                    .unwrap()
            })
            .await;

        assert_eq!(value, 42);
        assert_eq!(CACHE_CALLS.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_flow() {
        let _db = setup_db().await;
        ALWAYS_CALLS.store(0, Ordering::SeqCst);
        FLAKY_CALLS.store(0, Ordering::SeqCst);

        let input = serde_json::to_string(&()).unwrap();
        let handle = start_named(&format!("{}::{}", module_path!(), "test_workflow"), &input)
            .await
            .expect("start workflow");

        let claimed = store::mark_running(handle.id(), "test-worker", Duration::from_secs(30))
            .await
            .expect("mark running");

        let config = WorkflowConfig::from_env();
        process_claimed_workflow(claimed, Arc::new(config), "test-worker")
            .await
            .expect("process workflow");

        let status = store::get_workflow_status(handle.id()).await.unwrap();
        assert_eq!(status, WorkflowStatus::Pending);

        let claimed = store::mark_running(handle.id(), "test-worker", Duration::from_secs(30))
            .await
            .expect("mark running again");

        let config = WorkflowConfig::from_env();
        process_claimed_workflow(claimed, Arc::new(config), "test-worker")
            .await
            .expect("process workflow again");

        let status = store::get_workflow_status(handle.id()).await.unwrap();
        assert_eq!(status, WorkflowStatus::Succeeded);
        assert_eq!(ALWAYS_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(FLAKY_CALLS.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_name_normalization() {
        let _db = setup_db().await;

        let handle = start_workflow!(name_norm_workflow, 5)
            .await
            .expect("start workflow macro");

        let record = store::get_workflow_record(handle.id()).await.unwrap();
        let expected = format!("{}::{}", module_path!(), "name_norm_workflow");
        assert_eq!(record.name, expected);
    }

    async fn setup_db() -> TestDatabase {
        TestDatabase::fresh::<TestMigrator>()
            .await
            .expect("test db")
    }

    pub struct TestMigrator;

    #[async_trait::async_trait]
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![
                Box::new(CreateWorkflowsTable),
                Box::new(CreateWorkflowStepsTable),
            ]
        }
    }

    pub struct CreateWorkflowsTable;

    impl MigrationName for CreateWorkflowsTable {
        // Explicit, file-stable version. `DeriveMigrationName` derives from
        // the parent module path, which collides with `CreateWorkflowStepsTable`
        // because both live in the same `tests` module.
        fn name(&self) -> &str {
            "m20240101_000001_create_workflows"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for CreateWorkflowsTable {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .create_table(
                    Table::create()
                        .table(Workflows::Table)
                        .if_not_exists()
                        .col(
                            ColumnDef::new(Workflows::Id)
                                .big_integer()
                                .not_null()
                                .auto_increment()
                                .primary_key(),
                        )
                        .col(ColumnDef::new(Workflows::Name).string().not_null())
                        .col(ColumnDef::new(Workflows::Status).string().not_null())
                        .col(ColumnDef::new(Workflows::Input).text().not_null())
                        .col(ColumnDef::new(Workflows::Output).text().null())
                        .col(ColumnDef::new(Workflows::Error).text().null())
                        .col(ColumnDef::new(Workflows::Attempts).integer().not_null())
                        .col(ColumnDef::new(Workflows::MaxAttempts).integer().not_null())
                        .col(ColumnDef::new(Workflows::NextRunAt).timestamp().null())
                        .col(ColumnDef::new(Workflows::LockedUntil).timestamp().null())
                        .col(ColumnDef::new(Workflows::WorkerId).string().null())
                        .col(
                            ColumnDef::new(Workflows::CreatedAt)
                                .timestamp()
                                .not_null()
                                .default(Expr::current_timestamp()),
                        )
                        .col(
                            ColumnDef::new(Workflows::UpdatedAt)
                                .timestamp()
                                .not_null()
                                .default(Expr::current_timestamp()),
                        )
                        .col(ColumnDef::new(Workflows::StartedAt).timestamp().null())
                        .col(ColumnDef::new(Workflows::CompletedAt).timestamp().null())
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_workflows_status")
                        .table(Workflows::Table)
                        .col(Workflows::Status)
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_workflows_next_run_at")
                        .table(Workflows::Table)
                        .col(Workflows::NextRunAt)
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_workflows_locked_until")
                        .table(Workflows::Table)
                        .col(Workflows::LockedUntil)
                        .to_owned(),
                )
                .await
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(Workflows::Table).to_owned())
                .await
        }
    }

    pub struct CreateWorkflowStepsTable;

    impl MigrationName for CreateWorkflowStepsTable {
        fn name(&self) -> &str {
            "m20240101_000002_create_workflow_steps"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for CreateWorkflowStepsTable {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .create_table(
                    Table::create()
                        .table(WorkflowSteps::Table)
                        .if_not_exists()
                        .col(
                            ColumnDef::new(WorkflowSteps::Id)
                                .big_integer()
                                .not_null()
                                .auto_increment()
                                .primary_key(),
                        )
                        .col(
                            ColumnDef::new(WorkflowSteps::WorkflowId)
                                .big_integer()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(WorkflowSteps::StepIndex)
                                .integer()
                                .not_null(),
                        )
                        .col(ColumnDef::new(WorkflowSteps::StepName).string().not_null())
                        .col(ColumnDef::new(WorkflowSteps::Status).string().not_null())
                        .col(ColumnDef::new(WorkflowSteps::Input).text().not_null())
                        .col(ColumnDef::new(WorkflowSteps::Output).text().null())
                        .col(ColumnDef::new(WorkflowSteps::Error).text().null())
                        .col(ColumnDef::new(WorkflowSteps::Attempts).integer().not_null())
                        .col(
                            ColumnDef::new(WorkflowSteps::CreatedAt)
                                .timestamp()
                                .not_null()
                                .default(Expr::current_timestamp()),
                        )
                        .col(
                            ColumnDef::new(WorkflowSteps::UpdatedAt)
                                .timestamp()
                                .not_null()
                                .default(Expr::current_timestamp()),
                        )
                        .col(ColumnDef::new(WorkflowSteps::StartedAt).timestamp().null())
                        .col(
                            ColumnDef::new(WorkflowSteps::CompletedAt)
                                .timestamp()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_workflow_steps_workflow_id")
                        .table(WorkflowSteps::Table)
                        .col(WorkflowSteps::WorkflowId)
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_workflow_steps_unique")
                        .table(WorkflowSteps::Table)
                        .col(WorkflowSteps::WorkflowId)
                        .col(WorkflowSteps::StepIndex)
                        .unique()
                        .to_owned(),
                )
                .await
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(WorkflowSteps::Table).to_owned())
                .await
        }
    }

    #[derive(DeriveIden)]
    enum Workflows {
        Table,
        Id,
        Name,
        Status,
        Input,
        Output,
        Error,
        Attempts,
        MaxAttempts,
        NextRunAt,
        LockedUntil,
        WorkerId,
        CreatedAt,
        UpdatedAt,
        StartedAt,
        CompletedAt,
    }

    #[derive(DeriveIden)]
    enum WorkflowSteps {
        Table,
        Id,
        WorkflowId,
        StepIndex,
        StepName,
        Status,
        Input,
        Output,
        Error,
        Attempts,
        CreatedAt,
        UpdatedAt,
        StartedAt,
        CompletedAt,
    }
}
