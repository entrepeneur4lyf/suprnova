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
use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use tokio::task::JoinSet;

/// Element type stored in the [`JoinSet`] returned/consumed by the
/// `*_into` task-run methods. `String` is the task name and the inner
/// result is the task's handler outcome (panics are caught and surfaced
/// as `Err(FrameworkError::internal(...))` with the task name).
pub type ScheduledTaskJoin = (String, Result<(), FrameworkError>);

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

    /// Run all due tasks once.
    ///
    /// Tasks marked [`TaskBuilder::run_in_background`] are spawned and awaited
    /// internally before this function returns; inline tasks are awaited
    /// sequentially. The returned vector contains every task's name and
    /// result, regardless of how it was executed.
    ///
    /// Callers that need to keep background tasks running past a tick (the
    /// `schedule:work` daemon) should drive [`run_due_tasks_into`] with a
    /// long-lived [`JoinSet`] instead.
    pub async fn run_due_tasks(&self) -> Vec<ScheduledTaskJoin> {
        let mut joinset: JoinSet<ScheduledTaskJoin> = JoinSet::new();
        let mut results = self.run_due_tasks_into(&mut joinset).await;
        drain_joinset_into(&mut joinset, &mut results).await;
        results
    }

    /// Same as [`run_due_tasks`] but routes background tasks into the supplied
    /// `joinset` instead of awaiting them locally.
    ///
    /// Returns only the inline (non-background) results — caller is
    /// responsible for draining `joinset` to observe background-task
    /// outcomes. Background tasks are wrapped with `catch_unwind` so a panic
    /// surfaces as `Err(FrameworkError::internal(...))` carrying the task
    /// name; it never aborts the schedule's tick loop.
    pub async fn run_due_tasks_into(
        &self,
        joinset: &mut JoinSet<ScheduledTaskJoin>,
    ) -> Vec<ScheduledTaskJoin> {
        run_tasks_into(self.due_tasks(), joinset).await
    }

    /// Run every registered task once, regardless of schedule. Background
    /// tasks are awaited internally before returning. Useful for testing and
    /// manual triggering.
    pub async fn run_all_tasks(&self) -> Vec<ScheduledTaskJoin> {
        let mut joinset: JoinSet<ScheduledTaskJoin> = JoinSet::new();
        let mut results = self.run_all_tasks_into(&mut joinset).await;
        drain_joinset_into(&mut joinset, &mut results).await;
        results
    }

    /// `run_all_tasks` variant that pushes background tasks into the caller's
    /// `joinset` instead of awaiting them locally. Symmetric with
    /// [`run_due_tasks_into`].
    pub async fn run_all_tasks_into(
        &self,
        joinset: &mut JoinSet<ScheduledTaskJoin>,
    ) -> Vec<ScheduledTaskJoin> {
        run_tasks_into(self.tasks.iter(), joinset).await
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

/// Common body shared by [`Schedule::run_due_tasks_into`] and
/// [`Schedule::run_all_tasks_into`].
///
/// Inline tasks are awaited sequentially and their results returned.
/// Background tasks ([`TaskEntry::run_in_background`] is `true`) are spawned
/// into `joinset` via `tokio::spawn`, with `catch_unwind` so a handler panic
/// is converted into `Err(FrameworkError::internal(...))` carrying the task
/// name — the scheduler tick loop is never unwound by user code.
async fn run_tasks_into<'a, I>(
    tasks: I,
    joinset: &mut JoinSet<ScheduledTaskJoin>,
) -> Vec<ScheduledTaskJoin>
where
    I: IntoIterator<Item = &'a TaskEntry>,
{
    let mut inline = Vec::new();
    for task in tasks {
        if task.run_in_background {
            let name = task.name.clone();
            let panic_name = name.clone();
            let handler: BoxedTask = Arc::clone(&task.task);
            joinset.spawn(async move {
                let outcome = AssertUnwindSafe(async move { handler.handle().await })
                    .catch_unwind()
                    .await;
                let result = match outcome {
                    Ok(r) => r,
                    Err(_payload) => Err(FrameworkError::internal(format!(
                        "scheduled task '{panic_name}' panicked"
                    ))),
                };
                (name, result)
            });
        } else {
            let result = task.run().await;
            inline.push((task.name.clone(), result));
        }
    }
    inline
}

/// Drain every remaining task in `joinset` and append its result to `out`.
/// A [`tokio::task::JoinError`] (cancellation; panics are already converted
/// inside the spawned future) is surfaced as a synthetic
/// `(name="<unknown>", Err(FrameworkError::internal(...)))` so callers never
/// silently drop a task's outcome.
async fn drain_joinset_into(
    joinset: &mut JoinSet<ScheduledTaskJoin>,
    out: &mut Vec<ScheduledTaskJoin>,
) {
    while let Some(joined) = joinset.join_next().await {
        match joined {
            Ok(pair) => out.push(pair),
            Err(e) => out.push((
                "<unknown>".to_string(),
                Err(FrameworkError::internal(format!(
                    "scheduled task join error: {e}"
                ))),
            )),
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

    // -------------------------------------------------------------------------
    // run_in_background enforcement
    // -------------------------------------------------------------------------

    /// Two `run_in_background` tasks must actually run concurrently. A
    /// `tokio::sync::Barrier` requiring 2 waiters proves it: if the runtime
    /// awaited the tasks sequentially the first `barrier.wait()` would block
    /// forever waiting for the second waiter, which hasn't started; the test
    /// would time out. A successful return under the timeout proves both
    /// tasks were spawned and progressed in parallel.
    #[tokio::test]
    async fn run_in_background_spawns_tasks_concurrently() {
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut schedule = Schedule::new();
        for name in ["bg-1", "bg-2"] {
            let b = barrier.clone();
            let builder = schedule
                .call(move || {
                    let b = b.clone();
                    async move {
                        b.wait().await;
                        Ok(())
                    }
                })
                .every_minute()
                .name(name)
                .run_in_background();
            schedule.add(builder);
        }

        let results = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            schedule.run_due_tasks(),
        )
        .await
        .expect("background tasks must run concurrently — barrier(2) would deadlock if sequential");

        assert_eq!(results.len(), 2);
        for (name, r) in results {
            assert!(r.is_ok(), "task '{name}' should have completed Ok");
        }
    }

    /// A panicking `run_in_background` task must surface as `Err(...)`
    /// carrying the task name, NOT unwind the scheduler. The catch_unwind
    /// inside the spawn body is the safety net.
    #[tokio::test]
    async fn run_in_background_panic_is_isolated_and_named() {
        let mut schedule = Schedule::new();
        let b = schedule
            .call(|| async {
                panic!("intentional panic for isolation test");
            })
            .every_minute()
            .name("panicky")
            .run_in_background();
        schedule.add(b);
        let b = schedule
            .call(|| async { Ok(()) })
            .every_minute()
            .name("survivor")
            .run_in_background();
        schedule.add(b);

        let results = schedule.run_due_tasks().await;
        assert_eq!(results.len(), 2);

        let by_name: std::collections::BTreeMap<_, _> =
            results.iter().map(|(n, r)| (n.as_str(), r)).collect();

        let panicky = by_name
            .get("panicky")
            .expect("panicky task must appear in results");
        match panicky {
            Err(e) => assert!(
                e.to_string().contains("panicked"),
                "panic message should be surfaced: {e}",
            ),
            Ok(()) => panic!("panicking task must NOT produce Ok"),
        }
        let survivor = by_name
            .get("survivor")
            .expect("survivor task must still complete");
        assert!(
            survivor.is_ok(),
            "panic in one background task must not abort another",
        );
    }

    /// Inline tasks (no `run_in_background`) keep their original
    /// sequential semantics — used as a regression test against the
    /// `_into` plumbing accidentally spawning everything.
    #[tokio::test]
    async fn inline_tasks_run_sequentially() {
        let order = Arc::new(std::sync::Mutex::new(Vec::<&'static str>::new()));
        let mut schedule = Schedule::new();

        for name in ["a", "b", "c"] {
            let order = order.clone();
            let builder = schedule
                .call(move || {
                    let order = order.clone();
                    async move {
                        order.lock().unwrap().push(name);
                        Ok(())
                    }
                })
                .every_minute()
                .name(name);
            schedule.add(builder);
        }

        let results = schedule.run_due_tasks().await;
        assert_eq!(results.len(), 3);
        let order_snapshot = order.lock().unwrap().clone();
        assert_eq!(order_snapshot, vec!["a", "b", "c"]);
    }

    /// `run_due_tasks_into` returns ONLY inline results; background ones
    /// must land in the supplied JoinSet so the daemon's long-lived JoinSet
    /// works.
    #[tokio::test]
    async fn run_due_tasks_into_routes_background_into_joinset() {
        let mut schedule = Schedule::new();
        let b = schedule
            .call(|| async { Ok(()) })
            .every_minute()
            .name("inline");
        schedule.add(b);
        let b = schedule
            .call(|| async { Ok(()) })
            .every_minute()
            .name("backgrounded")
            .run_in_background();
        schedule.add(b);

        let mut js: JoinSet<ScheduledTaskJoin> = JoinSet::new();
        let inline = schedule.run_due_tasks_into(&mut js).await;
        assert_eq!(inline.len(), 1, "only inline results return from _into");
        assert_eq!(inline[0].0, "inline");

        // Background task is still pending in the JoinSet.
        let mut bg_results = Vec::new();
        while let Some(joined) = js.join_next().await {
            bg_results.push(joined.expect("background spawn must not yield JoinError"));
        }
        assert_eq!(bg_results.len(), 1);
        assert_eq!(bg_results[0].0, "backgrounded");
        assert!(bg_results[0].1.is_ok());
    }
}
