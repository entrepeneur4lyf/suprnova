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
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::time::Duration;

/// Default overlap-lock TTL: 30 minutes. Long enough that most scheduled
/// jobs finish well before it expires, short enough that a crashed task
/// holding an in-flight lock unblocks the next tick without operator
/// intervention. Override per task with
/// [`super::TaskBuilder::without_overlapping_for`].
pub const DEFAULT_WITHOUT_OVERLAPPING_TTL: Duration = Duration::from_secs(30 * 60);

/// Per-task runtime state shared between schedule entries and any spawned
/// background futures derived from them.
///
/// Holds counters needed to enforce [`TaskBuilder::without_overlapping`] in
/// the absence of a distributed [`Cache`] lock. Wrap in `Arc` so the same
/// instance is observed by the inline call path and any `tokio::spawn`
/// children — they need a shared view of whether a previous run is still
/// in flight.
///
/// [`TaskBuilder::without_overlapping`]: super::TaskBuilder::without_overlapping
/// [`Cache`]: crate::cache::Cache
#[derive(Default)]
pub struct TaskState {
    /// In-process running flag flipped via CAS when a task enters
    /// [`super::TaskEntry::run`] under `without_overlapping = true` without a
    /// usable [`Cache`] lock. Reset on completion regardless of result.
    ///
    /// [`Cache`]: crate::cache::Cache
    pub(crate) in_process_running: AtomicBool,
    /// Number of times this task has been observed and skipped due to an
    /// overlap lock (Cache-side or in-process) **or** because the
    /// same-minute dedup CAS rejected a repeat invocation. Read via
    /// [`TaskState::skip_count`] — the field stays `pub(crate)` so the
    /// atomic implementation can change without breaking external code.
    pub(crate) skip_count: AtomicUsize,
    /// Minutes-since-UNIX-epoch of the most recent invocation attempt.
    /// `fetch_max` against the current minute is the same-minute dedup
    /// gate — if the prior value is `>= now`, we already tried this minute
    /// and the new call must skip. Init to `0`: any post-epoch run wins
    /// the first CAS unconditionally.
    pub(crate) last_run_minute: AtomicI64,
}

impl TaskState {
    /// Build a fresh, idle [`TaskState`] wrapped in `Arc` so the builder
    /// can clone it into both the [`TaskEntry`] and any spawned background
    /// future.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Snapshot the skip counter — convenient for tests that need to assert
    /// "this task was skipped N times" without unwrapping atomics.
    pub fn skip_count(&self) -> usize {
        self.skip_count.load(Ordering::SeqCst)
    }
}

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
    /// TTL applied to the overlap lock when `without_overlapping` is set.
    /// Acts as a safety net for crashed tasks that fail to release the
    /// lock — the next tick after this duration sees a fresh lock and can
    /// proceed.
    pub overlap_ttl: Duration,
    /// Shared runtime state — in-process overlap flag and skip counter.
    pub state: Arc<TaskState>,
}

impl TaskEntry {
    /// Check if this task is due to run now
    pub fn is_due(&self) -> bool {
        self.expression.is_due()
    }

    /// Run the task, honouring `without_overlapping` if it is set.
    ///
    /// When the flag is enabled the executor first tries a distributed
    /// [`Cache::lock`] (so multi-process deployments coordinate); when
    /// `Cache` is not bootstrapped the executor degrades to a per-process
    /// `AtomicBool` CAS and emits a single warn-once telling the operator
    /// they're getting the weaker guarantee. A contended lock is treated
    /// as a successful skip — the task returns `Ok(())` and increments
    /// the [`TaskState`] skip counter so observability surfaces can see
    /// it without poisoning the `schedule:run` exit code.
    ///
    /// [`Cache::lock`]: crate::cache::Cache::lock
    pub async fn run(&self) -> TaskResult {
        run_handler_with_optional_overlap_guard(
            &self.name,
            Arc::clone(&self.task),
            self.without_overlapping,
            self.overlap_ttl,
            Arc::clone(&self.state),
        )
        .await
    }

    /// Get a human-readable description of the schedule
    pub fn schedule_description(&self) -> &str {
        self.expression.expression()
    }
}

/// Single warn-once latch for the "Cache not installed, falling back to
/// in-process overlap protection" message. Mirrors the precedent in
/// `features::middleware::warn_once_if_no_evaluator` so production logs
/// don't get flooded on every minute-aligned tick.
static CACHE_FALLBACK_WARNED: AtomicBool = AtomicBool::new(false);

fn warn_cache_fallback_once() {
    if !CACHE_FALLBACK_WARNED.swap(true, Ordering::SeqCst) {
        tracing::warn!(
            target: "suprnova::schedule",
            "without_overlapping() falling back to in-process AtomicBool protection — \
             Cache is not bootstrapped. Multi-process deployments (multiple `schedule:work` \
             or external-cron `schedule:run` callers) will NOT see each other's locks. \
             Configure Cache (CACHE_DRIVER=memory|redis) before relying on cross-process \
             overlap protection."
        );
    }
}

/// Shared implementation used by both [`TaskEntry::run`] (inline) and the
/// `tokio::spawn`'d background path in `schedule::run_tasks_into`. Pulled out
/// as a free function so the spawned `async move` future can capture the
/// `'static` arguments it needs without borrowing from `&TaskEntry`.
pub(crate) async fn run_handler_with_optional_overlap_guard(
    name: &str,
    handler: BoxedTask,
    without_overlapping: bool,
    overlap_ttl: Duration,
    state: Arc<TaskState>,
) -> TaskResult {
    // Same-minute dedup (always on, regardless of `without_overlapping`).
    // `fetch_max` returns the previous value and atomically bumps the
    // stored value to the max of (prev, now). If the previous value was
    // already at-or-past `now_minute`, this minute has already been
    // claimed — skip silently with a tick to `skip_count`. The audit's
    // HIGH #3 case (a daemon loop or repeated `schedule:run` invocation
    // executing the same minute-level task multiple times) is closed at
    // this gate; cross-process protection is layered on by Cache::lock
    // inside the `without_overlapping` branch below.
    let now_minute = chrono::Local::now().timestamp() / 60;
    let prev_minute = state
        .last_run_minute
        .fetch_max(now_minute, Ordering::SeqCst);
    if prev_minute >= now_minute {
        tracing::info!(
            target: "suprnova::schedule",
            task = %name,
            "skipped: already attempted for minute {now_minute}",
        );
        state.skip_count.fetch_add(1, Ordering::SeqCst);
        return Ok(());
    }

    if !without_overlapping {
        return handler.handle().await;
    }
    let lock_key = format!("schedule:lock:{name}");
    match crate::cache::Cache::lock(&lock_key, overlap_ttl).await {
        Ok(Some(guard)) => {
            let result = handler.handle().await;
            if let Err(e) = guard.release().await {
                tracing::warn!(
                    target: "suprnova::schedule",
                    error = %e,
                    "schedule: failed to release task lock; it will expire via TTL",
                );
            }
            result
        }
        Ok(None) => {
            tracing::info!(
                target: "suprnova::schedule",
                task = %name,
                "skipped: previous run still holds the overlap lock",
            );
            state.skip_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        Err(_) => {
            // Cache isn't bootstrapped — degrade to in-process CAS. Warn
            // operator once that they're getting the weaker guarantee.
            warn_cache_fallback_once();
            if state
                .in_process_running
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                let result = handler.handle().await;
                state.in_process_running.store(false, Ordering::SeqCst);
                result
            } else {
                tracing::info!(
                    target: "suprnova::schedule",
                    task = %name,
                    "skipped: in-process overlap flag already set",
                );
                state.skip_count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }
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
            overlap_ttl: DEFAULT_WITHOUT_OVERLAPPING_TTL,
            state: TaskState::new(),
        };

        assert_eq!(entry.name, "test-task");
        assert_eq!(entry.schedule_description(), "* * * * *");

        let result = entry.run().await;
        assert!(result.is_ok());
    }
}
