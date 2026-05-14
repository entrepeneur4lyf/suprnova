//! Scheduled task trait and entry types
//!
//! This module defines the `Task` trait for creating struct-based
//! scheduled tasks, as well as internal types for task management.

use super::expression::CronExpression;
use crate::error::FrameworkError;
use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Type alias for boxed task handlers
pub type BoxedTask = Arc<dyn TaskHandler + Send + Sync>;

/// Type alias for async task result
pub type TaskResult = Result<(), FrameworkError>;

/// Type alias for boxed future result
pub type BoxedFuture<'a> = Pin<Box<dyn Future<Output = TaskResult> + Send + 'a>>;

/// Internal trait for task execution
///
/// This trait is implemented automatically for `Task` and closure-based tasks.
#[async_trait]
pub trait TaskHandler: Send + Sync {
    /// Execute the task
    async fn handle(&self) -> TaskResult;
}

/// Trait for defining scheduled tasks
///
/// Implement this trait on a struct to create a reusable scheduled task.
/// Schedule configuration is done via the fluent builder API when registering.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{Task, TaskResult};
/// use async_trait::async_trait;
///
/// pub struct CleanupLogsTask;
///
/// impl CleanupLogsTask {
///     pub fn new() -> Self {
///         Self
///     }
/// }
///
/// #[async_trait]
/// impl Task for CleanupLogsTask {
///     async fn handle(&self) -> TaskResult {
///         // Cleanup logic here
///         println!("Cleaning up old log files...");
///         Ok(())
///     }
/// }
///
/// // Register in schedule.rs with fluent API:
/// // schedule.add(
/// //     schedule.task(CleanupLogsTask::new())
/// //         .daily()
/// //         .at("03:00")
/// //         .name("cleanup:logs")
/// // );
/// ```
#[async_trait]
pub trait Task: Send + Sync {
    /// Execute the task
    async fn handle(&self) -> TaskResult;
}

// Implement TaskHandler for any type implementing Task
#[async_trait]
impl<T: Task> TaskHandler for T {
    async fn handle(&self) -> TaskResult {
        Task::handle(self).await
    }
}

/// A registered task entry in the schedule
///
/// This struct holds all the information about a scheduled task,
/// including its schedule expression, configuration, and the task itself.
pub struct TaskEntry {
    /// Unique name for the task
    pub name: String,
    /// Cron expression defining when the task runs
    pub expression: CronExpression,
    /// The task handler
    pub task: BoxedTask,
    /// Optional description
    pub description: Option<String>,
    /// Prevent overlapping runs
    pub without_overlapping: bool,
    /// Run in background (non-blocking)
    pub run_in_background: bool,
}

impl TaskEntry {
    /// Check if this task is due to run now
    pub fn is_due(&self) -> bool {
        self.expression.is_due()
    }

    /// Run the task
    pub async fn run(&self) -> TaskResult {
        self.task.handle().await
    }

    /// Get a human-readable description of the schedule
    pub fn schedule_description(&self) -> &str {
        self.expression.expression()
    }
}

/// Wrapper for closure-based tasks
pub(crate) struct ClosureTask<F>
where
    F: Fn() -> BoxedFuture<'static> + Send + Sync,
{
    pub(crate) handler: F,
}

#[async_trait]
impl<F> TaskHandler for ClosureTask<F>
where
    F: Fn() -> BoxedFuture<'static> + Send + Sync,
{
    async fn handle(&self) -> TaskResult {
        (self.handler)().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestTask;

    #[async_trait]
    impl Task for TestTask {
        async fn handle(&self) -> TaskResult {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_task_trait() {
        let task = TestTask;

        let result: TaskResult = Task::handle(&task).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_task_entry() {
        let task = TestTask;
        let entry = TaskEntry {
            name: "test-task".to_string(),
            expression: CronExpression::every_minute(),
            task: Arc::new(task),
            description: Some("A test task".to_string()),
            without_overlapping: false,
            run_in_background: false,
        };

        assert_eq!(entry.name, "test-task");
        assert_eq!(entry.schedule_description(), "* * * * *");

        let result = entry.run().await;
        assert!(result.is_ok());
    }
}
