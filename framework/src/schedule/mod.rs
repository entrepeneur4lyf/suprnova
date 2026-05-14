//! Task Scheduler module for suprnova framework
//!
//! Provides a Laravel-like task scheduling system with support for:
//! - Trait-based tasks (implement `Task`)
//! - Closure-based tasks (inline definitions)
//! - Fluent scheduling API (`.daily()`, `.hourly()`, etc.)
//!
//! # Quick Start
//!
//! ## Using Trait-Based Tasks
//!
//! ```rust,ignore
//! use suprnova::{Task, TaskResult};
//! use async_trait::async_trait;
//!
//! pub struct CleanupLogsTask;
//!
//! #[async_trait]
//! impl Task for CleanupLogsTask {
//!     async fn handle(&self) -> TaskResult {
//!         // Your task logic here
//!         Ok(())
//!     }
//! }
//!
//! // Register in schedule.rs with fluent API
//! pub fn register(schedule: &mut Schedule) {
//!     schedule.add(
//!         schedule.task(CleanupLogsTask)
//!             .daily()
//!             .at("03:00")
//!             .name("cleanup:logs")
//!     );
//! }
//! ```
//!
//! ## Using Closure-Based Tasks
//!
//! ```rust,ignore
//! use suprnova::Schedule;
//!
//! pub fn register(schedule: &mut Schedule) {
//!     // Simple closure task
//!     schedule.add(
//!         schedule.call(|| async {
//!             println!("Running every minute!");
//!             Ok(())
//!         }).every_minute().name("minute-task")
//!     );
//!
//!     // Configured closure task
//!     schedule.add(
//!         schedule.call(|| async {
//!             println!("Daily cleanup!");
//!             Ok(())
//!         })
//!         .daily()
//!         .at("03:00")
//!         .name("daily-cleanup")
//!         .description("Cleans up temporary files")
//!     );
//! }
//! ```
//!
//! # Running the Scheduler
//!
//! Use the CLI commands to run scheduled tasks:
//!
//! ```bash
//! # Run due tasks once (for cron)
//! suprnova schedule:run
//!
//! # Run as daemon (continuous)
//! suprnova schedule:work
//!
//! # List all scheduled tasks
//! suprnova schedule:list
//! ```

pub mod builder;
pub mod expression;
pub mod task;

pub use builder::TaskBuilder;
pub use expression::{CronExpression, DayOfWeek};
pub use task::{BoxedFuture, BoxedTask, Task, TaskEntry, TaskHandler, TaskResult};

use crate::error::FrameworkError;

/// Schedule - main entry point for scheduling tasks
///
/// Provides methods for registering and running scheduled tasks.
/// Tasks can be registered via trait implementations or closures.
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::Schedule;
///
/// pub fn register(schedule: &mut Schedule) {
///     // Register a struct implementing Task trait
///     schedule.add(
///         schedule.task(MyCleanupTask::new())
///             .daily()
///             .at("03:00")
///             .name("cleanup")
///     );
///
///     // Or use a closure
///     schedule.add(
///         schedule.call(|| async {
///             println!("Hello!");
///             Ok(())
///         }).daily().at("03:00").name("greeting")
///     );
/// }
/// ```
pub struct Schedule {
    tasks: Vec<TaskEntry>,
}

impl Schedule {
    /// Create a new empty schedule
    pub fn new() -> Self {
        Self { tasks: Vec::new() }
    }

    /// Register a trait-based scheduled task
    ///
    /// Returns a `TaskBuilder` that allows fluent schedule configuration.
    ///
    /// # Example
    /// ```rust,ignore
    /// schedule.add(
    ///     schedule.task(CleanupLogsTask::new())
    ///         .daily()
    ///         .at("03:00")
    ///         .name("cleanup:logs")
    /// );
    /// ```
    pub fn task<T: Task + 'static>(&self, task: T) -> TaskBuilder {
        TaskBuilder::from_task(task)
    }

    /// Register a closure-based scheduled task
    ///
    /// Returns a `TaskBuilder` that allows you to configure the schedule
    /// using a fluent API.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// schedule.call(|| async {
    ///     // Task logic here
    ///     Ok(())
    /// }).daily().at("03:00").name("my-task");
    /// ```
    pub fn call<F, Fut>(&mut self, f: F) -> TaskBuilder
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), FrameworkError>> + Send + 'static,
    {
        TaskBuilder::from_async(f)
    }

    /// Add a configured task builder to the schedule
    ///
    /// This method is typically called after configuring a task with `call()`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let builder = schedule.call(|| async { Ok(()) }).daily();
    /// schedule.add(builder);
    /// ```
    pub fn add(&mut self, builder: TaskBuilder) -> &mut Self {
        let task_index = self.tasks.len();
        self.tasks.push(builder.build(task_index));
        self
    }

    /// Get all registered tasks
    pub fn tasks(&self) -> &[TaskEntry] {
        &self.tasks
    }

    /// Get the number of registered tasks
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Check if there are no registered tasks
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Get tasks that are due to run now
    pub fn due_tasks(&self) -> Vec<&TaskEntry> {
        self.tasks.iter().filter(|t| t.is_due()).collect()
    }

    /// Run all due tasks once
    ///
    /// Returns a vector of results for each task that was run.
    pub async fn run_due_tasks(&self) -> Vec<(&str, Result<(), FrameworkError>)> {
        let due = self.due_tasks();
        let mut results = Vec::new();

        for task in due {
            let result = task.run().await;
            results.push((task.name.as_str(), result));
        }

        results
    }

    /// Run all tasks regardless of their schedule
    ///
    /// Useful for testing or manual triggering.
    pub async fn run_all_tasks(&self) -> Vec<(&str, Result<(), FrameworkError>)> {
        let mut results = Vec::new();

        for task in &self.tasks {
            let result = task.run().await;
            results.push((task.name.as_str(), result));
        }

        results
    }

    /// Find a task by name
    pub fn find(&self, name: &str) -> Option<&TaskEntry> {
        self.tasks.iter().find(|t| t.name == name)
    }

    /// Run a specific task by name
    pub async fn run_task(&self, name: &str) -> Option<Result<(), FrameworkError>> {
        if let Some(task) = self.find(name) {
            Some(task.run().await)
        } else {
            None
        }
    }
}

impl Default for Schedule {
    fn default() -> Self {
        Self::new()
    }
}

/// Macro for creating closure-based tasks more ergonomically
///
/// # Example
///
/// ```rust,ignore
/// use suprnova::{schedule_task, Schedule};
///
/// pub fn register(schedule: &mut Schedule) {
///     schedule.add(
///         schedule_task!(|| async {
///             println!("Running!");
///             Ok(())
///         })
///         .daily()
///         .name("my-task")
///     );
/// }
/// ```
#[macro_export]
macro_rules! schedule_task {
    ($f:expr) => {
        $crate::schedule::TaskBuilder::from_async($f)
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct TestTask;

    #[async_trait]
    impl Task for TestTask {
        async fn handle(&self) -> Result<(), FrameworkError> {
            Ok(())
        }
    }

    #[test]
    fn test_schedule_new() {
        let schedule = Schedule::new();
        assert!(schedule.is_empty());
        assert_eq!(schedule.len(), 0);
    }

    #[test]
    fn test_schedule_add_trait_task() {
        let mut schedule = Schedule::new();
        schedule.add(schedule.task(TestTask).every_minute().name("test-1"));
        schedule.add(schedule.task(TestTask).every_minute().name("test-2"));

        assert_eq!(schedule.len(), 2);
        assert!(!schedule.is_empty());
    }

    #[test]
    fn test_schedule_add_closure_task() {
        let mut schedule = Schedule::new();

        let builder = schedule
            .call(|| async { Ok(()) })
            .daily()
            .name("closure-task");

        schedule.add(builder);

        assert_eq!(schedule.len(), 1);
    }

    #[test]
    fn test_schedule_find_task() {
        let mut schedule = Schedule::new();
        schedule.add(schedule.task(TestTask).every_minute().name("find-me"));

        let found = schedule.find("find-me");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "find-me");

        let not_found = schedule.find("not-exists");
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_schedule_run_task() {
        let mut schedule = Schedule::new();
        schedule.add(schedule.task(TestTask).every_minute().name("run-me"));

        let result = schedule.run_task("run-me").await;
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());

        let not_found = schedule.run_task("not-exists").await;
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_schedule_run_all_tasks() {
        let mut schedule = Schedule::new();
        schedule.add(schedule.task(TestTask).every_minute().name("task-1"));
        schedule.add(schedule.task(TestTask).every_minute().name("task-2"));

        let results = schedule.run_all_tasks().await;
        assert_eq!(results.len(), 2);

        for (name, result) in results {
            assert!(result.is_ok(), "Task {} failed", name);
        }
    }
}
