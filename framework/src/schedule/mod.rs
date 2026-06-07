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
    pub fn call<F, Fut>(&self, f: F) -> TaskBuilder
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
///
/// Both paths route through
/// [`task::run_handler_with_optional_overlap_guard`] so the
/// `without_overlapping` flag is honoured regardless of execution mode.
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
            let guard_name = name.clone();
            let handler: BoxedTask = Arc::clone(&task.task);
            let without_overlapping = task.without_overlapping;
            let overlap_ttl = task.overlap_ttl;
            let state = Arc::clone(&task.state);
            joinset.spawn(async move {
                let outcome = AssertUnwindSafe(async move {
                    task::run_handler_with_optional_overlap_guard(
                        &guard_name,
                        handler,
                        without_overlapping,
                        overlap_ttl,
                        state,
                    )
                    .await
                })
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
            // Inline tasks run on the caller's task; without a panic boundary
            // a panicking handler would unwind the scheduler daemon
            // (`schedule:work`) entirely. Mirror the background-spawn path:
            // catch the panic, convert it into a typed Err carrying the task
            // name, and push it into the inline result vec like any other
            // failure.
            let name = task.name.clone();
            let panic_name = name.clone();
            let result = match AssertUnwindSafe(task.run()).catch_unwind().await {
                Ok(r) => r,
                Err(payload) => {
                    let msg = crate::server::panic_payload_message(&payload);
                    Err(FrameworkError::internal(format!(
                        "scheduled task '{panic_name}' panicked: {msg}"
                    )))
                }
            };
            inline.push((name, result));
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

    /// A panicking *inline* task (no `run_in_background`) must surface as
    /// `Err(...)` carrying the task name, NOT unwind the `schedule:work`
    /// daemon. The background path already does this; before the inline
    /// panic boundary, a panic in `task.run().await` would unwind through
    /// `run_tasks_into` and tear the scheduler down. A subsequent inline
    /// task must still run.
    #[tokio::test]
    async fn inline_panic_is_isolated_and_named() {
        let mut schedule = Schedule::new();
        let b = schedule
            .call(|| async {
                panic!("intentional panic for inline isolation test");
            })
            .every_minute()
            .name("inline-panicky");
        schedule.add(b);
        let b = schedule
            .call(|| async { Ok(()) })
            .every_minute()
            .name("inline-survivor");
        schedule.add(b);

        let results = schedule.run_due_tasks().await;
        assert_eq!(results.len(), 2);

        let by_name: std::collections::BTreeMap<_, _> =
            results.iter().map(|(n, r)| (n.as_str(), r)).collect();

        let panicky = by_name
            .get("inline-panicky")
            .expect("panicky inline task must appear in results");
        match panicky {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("panicked"),
                    "panic must be surfaced as a typed error: {msg}",
                );
                assert!(
                    msg.contains("inline-panicky"),
                    "error message must name the panicking task: {msg}",
                );
            }
            Ok(()) => panic!("panicking inline task must NOT produce Ok"),
        }
        let survivor = by_name
            .get("inline-survivor")
            .expect("survivor inline task must still complete");
        assert!(
            survivor.is_ok(),
            "panic in one inline task must not abort the next",
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

    // -------------------------------------------------------------------------
    // without_overlapping enforcement
    // -------------------------------------------------------------------------

    /// Without Cache bootstrapped, the `without_overlapping` in-process
    /// AtomicBool must skip a second invocation while the first is still
    /// in the handler — across a (simulated) minute boundary so the
    /// same-minute CAS gate doesn't pre-empt the assertion.
    ///
    /// Design note: each registered task carries its own `Arc<TaskState>`
    /// — the overlap flag is per-task identity, not a global gate. The
    /// always-on same-minute CAS would otherwise fire first; we reset
    /// `last_run_minute` between drive 1 and drive 2 to simulate the
    /// minute rolling over so the in-process layer is exercised. The
    /// without_overlapping AtomicBool catches: drive 2 enters, finds the
    /// flag set, skips with `Ok(())` and ticks `skip_count`.
    #[tokio::test]
    async fn without_overlapping_in_process_fallback_skips_overlapping_call() {
        use crate::testing::TestContainer;
        // No CacheStore binding in this scope — Cache::lock will Err(...)
        // and the executor will fall back to in-process AtomicBool
        // protection.
        let _scope = TestContainer::fake();

        let start_signal = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut schedule = Schedule::new();
        let start_signal_c = start_signal.clone();
        let release_c = release.clone();
        let started_c = started.clone();
        let builder = schedule
            .call(move || {
                let start_signal = start_signal_c.clone();
                let release = release_c.clone();
                let started = started_c.clone();
                async move {
                    started.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    start_signal.notify_one();
                    release.notified().await;
                    Ok(())
                }
            })
            .every_minute()
            .name("singleton")
            .without_overlapping();
        schedule.add(builder);

        let state = schedule.tasks()[0].state.clone();
        let schedule = Arc::new(schedule);

        // Drive 1: starts the task, blocks waiting for `release`.
        let s1 = Arc::clone(&schedule);
        let drive1 = tokio::spawn(async move { s1.run_due_tasks().await });

        // Wait for the handler to confirm it's past the CAS guard.
        tokio::time::timeout(std::time::Duration::from_secs(2), start_signal.notified())
            .await
            .expect("first run must enter the handler");
        assert_eq!(started.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Simulate the minute rolling over so the always-on same-minute
        // CAS does not pre-empt the in-process AtomicBool we're trying to
        // exercise. Reset last_run_minute to 0 (the init value) — drive 2
        // will then see prev < now_minute, win the same-minute CAS, and
        // proceed to the without_overlapping branch.
        state
            .last_run_minute
            .store(0, std::sync::atomic::Ordering::SeqCst);

        // Drive 2: the AtomicBool is set, so the handler is not entered;
        // the call returns Ok(()) and skip_count ticks by one.
        let r2 = schedule.run_due_tasks().await;
        assert_eq!(r2.len(), 1);
        assert!(
            r2[0].1.is_ok(),
            "skipped run is reported as Ok(()) per Laravel parity",
        );
        assert_eq!(state.skip_count(), 1, "second call must register a skip");
        assert_eq!(
            started.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "skipped call MUST NOT enter the handler",
        );

        // Release drive 1 and confirm it completes cleanly. Flag reset
        // behaviour is covered separately by
        // `without_overlapping_in_process_flag_resets_after_each_run`.
        release.notify_one();
        let r1 = tokio::time::timeout(std::time::Duration::from_secs(2), drive1)
            .await
            .expect("first run must complete after release")
            .unwrap();
        assert_eq!(r1.len(), 1);
        assert!(r1[0].1.is_ok());
    }

    /// Sequential invocations across different minutes must release the
    /// in-process flag so the next run can proceed — the AtomicBool must
    /// reset whether the handler returned Ok or Err. We reset the
    /// same-minute CAS state between iterations to simulate the minute
    /// rolling over; the in-process AtomicBool reset is what we're
    /// asserting here.
    #[tokio::test]
    async fn without_overlapping_in_process_flag_resets_after_each_run() {
        use crate::testing::TestContainer;
        let _scope = TestContainer::fake();

        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut schedule = Schedule::new();
        let counter_clone = counter.clone();
        let builder = schedule
            .call(move || {
                let counter = counter_clone.clone();
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }
            })
            .every_minute()
            .name("repeatable")
            .without_overlapping();
        schedule.add(builder);

        for _ in 0..3 {
            // Simulate the minute rolling over so the always-on
            // same-minute CAS lets each iteration through to the
            // in-process AtomicBool layer we're actually asserting.
            schedule.tasks()[0]
                .state
                .last_run_minute
                .store(0, std::sync::atomic::Ordering::SeqCst);
            let results = schedule.run_due_tasks().await;
            assert_eq!(results.len(), 1);
            assert!(results[0].1.is_ok());
        }
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(schedule.tasks()[0].state.skip_count(), 0);
    }

    // -------------------------------------------------------------------------
    // same-minute dedup (HIGH 3)
    // -------------------------------------------------------------------------

    /// Two `run_due_tasks` calls within the same wall-clock minute must
    /// dedup the second one — closes the in-process subset of the
    /// audit's HIGH 3 case (cross-process external-cron same-minute
    /// dedup is the opt-in path via `without_overlapping` + a Cache
    /// backend; the always-on in-process CAS does not span processes).
    ///
    /// Test exercises the in-process gate directly: call once, observe
    /// the handler ran; call again immediately (same minute by
    /// construction since the test takes milliseconds); observe the
    /// handler did NOT run a second time and skip_count was bumped.
    ///
    /// Wall-clock margin: the two `run_due_tasks` calls execute in
    /// microseconds, so they land in the same UNIX minute with
    /// probability ≈1 − 8e-7 (≈ms-budget / 60s-budget). A flake here
    /// would mean a real-time minute boundary fired between the two
    /// calls — vanishingly unlikely and easy to retry.
    #[tokio::test]
    async fn same_minute_cas_dedups_repeated_call_within_same_minute() {
        use crate::testing::TestContainer;
        let _scope = TestContainer::fake();

        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut schedule = Schedule::new();
        let counter_clone = counter.clone();
        let builder = schedule
            .call(move || {
                let counter = counter_clone.clone();
                async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }
            })
            .every_minute()
            .name("dedup-target");
        schedule.add(builder);

        // First call: runs handler.
        let r1 = schedule.run_due_tasks().await;
        assert_eq!(r1.len(), 1);
        assert!(r1[0].1.is_ok());
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(schedule.tasks()[0].state.skip_count(), 0);

        // Second call within the same minute: same-minute CAS rejects;
        // handler is NOT invoked again; skip_count ticks by one.
        let r2 = schedule.run_due_tasks().await;
        assert_eq!(r2.len(), 1);
        assert!(
            r2[0].1.is_ok(),
            "same-minute skip is reported as Ok(()) per Laravel parity",
        );
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "handler MUST NOT fire twice within the same minute",
        );
        assert_eq!(schedule.tasks()[0].state.skip_count(), 1);

        // Simulate a minute boundary; next call should fire again.
        schedule.tasks()[0]
            .state
            .last_run_minute
            .store(0, std::sync::atomic::Ordering::SeqCst);
        let r3 = schedule.run_due_tasks().await;
        assert_eq!(r3.len(), 1);
        assert!(r3[0].1.is_ok());
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert_eq!(
            schedule.tasks()[0].state.skip_count(),
            1,
            "post-minute-rollover call MUST NOT skip",
        );
    }

    /// `CronExpression::is_due_at` lets tests drive cron evaluation
    /// against a fixed clock — the audit's test-coverage gap "Add
    /// clock-controlled tests for once-per-minute de-duplication,
    /// repeated `run_due_tasks`, and daemon tick behavior" depends on
    /// this hook. Pin both a matching and a non-matching minute.
    #[test]
    fn is_due_at_drives_cron_with_synthetic_clock() {
        use chrono::{Local, TimeZone as _};

        // Build an expression that fires at 03:00 every day.
        let expr = expression::CronExpression::daily().at("03:00");
        assert_eq!(expr.expression(), "0 3 * * *");

        // A synthetic clock pointing at 03:00 on some arbitrary day —
        // must report due.
        let due_clock = Local
            .with_ymd_and_hms(2026, 5, 28, 3, 0, 0)
            .single()
            .expect("test clock construction must yield a single instant");
        assert!(
            expr.is_due_at(due_clock),
            "0 3 * * * should be due at 2026-05-28 03:00:00 local",
        );

        // Same day, 03:01 — minute field doesn't match, must NOT be due.
        let off_clock = Local
            .with_ymd_and_hms(2026, 5, 28, 3, 1, 0)
            .single()
            .expect("test clock construction must yield a single instant");
        assert!(
            !expr.is_due_at(off_clock),
            "0 3 * * * must NOT be due at 03:01 (minute mismatch)",
        );

        // Same minute, different hour — must NOT be due.
        let wrong_hour = Local
            .with_ymd_and_hms(2026, 5, 28, 4, 0, 0)
            .single()
            .expect("test clock construction must yield a single instant");
        assert!(
            !expr.is_due_at(wrong_hour),
            "0 3 * * * must NOT be due at 04:00 (hour mismatch)",
        );
    }

    /// `without_overlapping_for` overrides the default TTL.
    #[test]
    fn without_overlapping_for_sets_custom_ttl() {
        let mut schedule = Schedule::new();
        let custom_ttl = std::time::Duration::from_secs(7);
        let builder = schedule
            .call(|| async { Ok(()) })
            .every_minute()
            .name("custom-ttl")
            .without_overlapping_for(custom_ttl);
        schedule.add(builder);
        let entry = &schedule.tasks()[0];
        assert!(entry.without_overlapping);
        assert_eq!(entry.overlap_ttl, custom_ttl);
    }

    /// Plain `without_overlapping` (no `_for`) uses the documented default
    /// of 30 minutes — pinned so future changes to the constant are seen.
    #[test]
    fn without_overlapping_uses_default_ttl_when_unspecified() {
        let mut schedule = Schedule::new();
        let builder = schedule
            .call(|| async { Ok(()) })
            .every_minute()
            .name("default-ttl")
            .without_overlapping();
        schedule.add(builder);
        let entry = &schedule.tasks()[0];
        assert!(entry.without_overlapping);
        assert_eq!(entry.overlap_ttl, task::DEFAULT_WITHOUT_OVERLAPPING_TTL);
        assert_eq!(
            entry.overlap_ttl,
            std::time::Duration::from_secs(30 * 60),
            "default TTL contract: 30 min",
        );
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
