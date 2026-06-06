//! Task builder for fluent schedule configuration
//!
//! Provides a fluent API for configuring scheduled tasks with closures.

use super::expression::{CronExpression, DayOfWeek};
use super::task::{
    BoxedFuture, BoxedTask, ClosureTask, DEFAULT_WITHOUT_OVERLAPPING_TTL, Task, TaskEntry,
    TaskResult, TaskState,
};
use std::sync::Arc;
use std::time::Duration;

/// Builder for configuring scheduled tasks with a fluent API
///
/// This builder is returned by `Schedule::call()` and allows you to configure
/// when and how a closure-based task should run.
///
/// # Example
///
/// ```rust,ignore
/// schedule.call(|| async {
///     println!("Running task!");
///     Ok(())
/// })
/// .daily()
/// .at("03:00")
/// .name("daily-task")
/// .description("Runs every day at 3 AM");
/// ```
pub struct TaskBuilder {
    pub(crate) task: BoxedTask,
    pub(crate) expression: CronExpression,
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) without_overlapping: bool,
    pub(crate) overlap_ttl: Option<Duration>,
    pub(crate) run_in_background: bool,
}

impl TaskBuilder {
    /// Create a new task builder with a closure
    ///
    /// The closure should return a future that resolves to `Result<(), FrameworkError>`.
    pub fn new<F>(f: F) -> Self
    where
        F: Fn() -> BoxedFuture<'static> + Send + Sync + 'static,
    {
        Self {
            task: Arc::new(ClosureTask { handler: f }),
            expression: CronExpression::every_minute(),
            name: None,
            description: None,
            without_overlapping: false,
            overlap_ttl: None,
            run_in_background: false,
        }
    }

    /// Create a TaskBuilder from an async closure
    ///
    /// This is a convenience method that wraps the async closure properly.
    pub fn from_async<F, Fut>(f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = TaskResult> + Send + 'static,
    {
        Self::new(move || Box::pin(f()))
    }

    /// Create a TaskBuilder from a struct implementing the Task trait
    ///
    /// This allows using the fluent schedule API with struct-based tasks.
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
    pub fn from_task<T: Task + 'static>(task: T) -> Self {
        Self {
            task: Arc::new(task),
            expression: CronExpression::every_minute(),
            name: None,
            description: None,
            without_overlapping: false,
            overlap_ttl: None,
            run_in_background: false,
        }
    }

    // =========================================================================
    // Schedule Expression Methods
    // =========================================================================

    /// Set a custom cron expression
    ///
    /// # Example
    /// ```rust,ignore
    /// .cron("0 */5 * * *") // Every 5 hours at minute 0
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if the cron expression is invalid (wrong field count, unparseable
    /// step / range / list / numeric segment). Use [`try_cron`](Self::try_cron)
    /// for a fallible alternative.
    pub fn cron(mut self, expression: &str) -> Self {
        self.expression = CronExpression::parse(expression).expect("Invalid cron expression");
        self
    }

    /// Fallible sibling of [`cron`](Self::cron): returns `Err` instead of
    /// panicking when the expression is invalid.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a descriptive message when [`CronExpression::parse`]
    /// rejects `expression` — i.e. when the expression does not have exactly
    /// 5 whitespace-separated fields, or any field contains an unparseable
    /// numeric segment (step, range bound, list element, or single value).
    pub fn try_cron(mut self, expression: &str) -> Result<Self, String> {
        self.expression = CronExpression::parse(expression)?;
        Ok(self)
    }

    /// Run every minute
    pub fn every_minute(mut self) -> Self {
        self.expression = CronExpression::every_minute();
        self
    }

    /// Run every 2 minutes
    pub fn every_two_minutes(mut self) -> Self {
        self.expression = CronExpression::every_n_minutes(2);
        self
    }

    /// Run every 5 minutes
    pub fn every_five_minutes(mut self) -> Self {
        self.expression = CronExpression::every_n_minutes(5);
        self
    }

    /// Run every 10 minutes
    pub fn every_ten_minutes(mut self) -> Self {
        self.expression = CronExpression::every_n_minutes(10);
        self
    }

    /// Run every 15 minutes
    pub fn every_fifteen_minutes(mut self) -> Self {
        self.expression = CronExpression::every_n_minutes(15);
        self
    }

    /// Run every 30 minutes
    pub fn every_thirty_minutes(mut self) -> Self {
        self.expression = CronExpression::every_n_minutes(30);
        self
    }

    /// Run every hour at minute 0
    pub fn hourly(mut self) -> Self {
        self.expression = CronExpression::hourly();
        self
    }

    /// Run every hour at a specific minute
    ///
    /// # Example
    /// ```rust,ignore
    /// .hourly_at(30) // Every hour at XX:30
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if `minute` is outside the cron minute range `0..=59`. Use
    /// [`try_hourly_at`](Self::try_hourly_at) for a fallible alternative.
    pub fn hourly_at(mut self, minute: u32) -> Self {
        self.expression = CronExpression::hourly_at(minute);
        self
    }

    /// Fallible sibling of [`hourly_at`](Self::hourly_at): returns `Err`
    /// instead of panicking when `minute` is outside `0..=59`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `minute` is outside `0..=59` (the cron minute
    /// field width). Delegates to [`CronExpression::try_hourly_at`].
    pub fn try_hourly_at(mut self, minute: u32) -> Result<Self, String> {
        self.expression = CronExpression::try_hourly_at(minute)?;
        Ok(self)
    }

    /// Run every 2 hours
    pub fn every_two_hours(mut self) -> Self {
        self.expression = CronExpression::parse("0 */2 * * *").unwrap();
        self
    }

    /// Run every 3 hours
    pub fn every_three_hours(mut self) -> Self {
        self.expression = CronExpression::parse("0 */3 * * *").unwrap();
        self
    }

    /// Run every 4 hours
    pub fn every_four_hours(mut self) -> Self {
        self.expression = CronExpression::parse("0 */4 * * *").unwrap();
        self
    }

    /// Run every 6 hours
    pub fn every_six_hours(mut self) -> Self {
        self.expression = CronExpression::parse("0 */6 * * *").unwrap();
        self
    }

    /// Run once daily at midnight
    pub fn daily(mut self) -> Self {
        self.expression = CronExpression::daily();
        self
    }

    /// Run daily at a specific time
    ///
    /// # Example
    /// ```rust,ignore
    /// .daily_at("13:00") // Daily at 1:00 PM
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if `time` is a well-formed `"HH:MM"` whose numeric segments
    /// are out of cron range (hour `0..=23`, minute `0..=59`). A non-`HH:MM`
    /// string falls back to [`daily`](Self::daily); a non-numeric segment
    /// is treated as `0`. Use [`try_daily_at`](Self::try_daily_at) for a
    /// fallible alternative.
    pub fn daily_at(mut self, time: &str) -> Self {
        self.expression = CronExpression::daily_at(time);
        self
    }

    /// Fallible sibling of [`daily_at`](Self::daily_at): returns `Err` instead
    /// of panicking when a numeric `HH:MM` segment is out of cron range.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `time` is a well-formed `"HH:MM"` whose hour is
    /// outside `0..=23` or whose minute is outside `0..=59`. Lenient parsing
    /// is preserved: a non-`HH:MM` string yields the equivalent of
    /// [`daily`](Self::daily); a non-numeric segment is treated as `0`.
    /// Delegates to [`CronExpression::try_daily_at`].
    pub fn try_daily_at(mut self, time: &str) -> Result<Self, String> {
        self.expression = CronExpression::try_daily_at(time)?;
        Ok(self)
    }

    /// Run twice daily at specific times
    ///
    /// # Example
    /// ```rust,ignore
    /// .twice_daily(1, 13) // At 1:00 AM and 1:00 PM
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if either `first_hour` or `second_hour` is outside the cron
    /// hour range `0..=23`. Use [`try_twice_daily`](Self::try_twice_daily)
    /// for a fallible alternative.
    pub fn twice_daily(self, first_hour: u32, second_hour: u32) -> Self {
        self.try_twice_daily(first_hour, second_hour)
            .expect("twice_daily: both hours must be in the cron hour range 0..=23")
    }

    /// Fallible sibling of [`twice_daily`](Self::twice_daily): returns `Err`
    /// instead of panicking when either hour is outside `0..=23`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when either `first_hour` or `second_hour` is outside
    /// `0..=23` (the cron hour field width).
    pub fn try_twice_daily(mut self, first_hour: u32, second_hour: u32) -> Result<Self, String> {
        if first_hour > 23 || second_hour > 23 {
            return Err(format!(
                "twice_daily: hours must be in 0..=23, got {first_hour} and {second_hour}"
            ));
        }
        self.expression =
            CronExpression::parse(&format!("0 {},{} * * *", first_hour, second_hour))?;
        Ok(self)
    }

    /// Set the time for the current schedule
    ///
    /// This can be chained with other methods to set a specific time.
    ///
    /// # Example
    /// ```rust,ignore
    /// .daily().at("14:30") // Daily at 2:30 PM
    /// .weekly().at("09:00") // Weekly at 9:00 AM
    /// ```
    pub fn at(mut self, time: &str) -> Self {
        self.expression = self.expression.at(time);
        self
    }

    /// Run once weekly on Sunday at midnight
    pub fn weekly(mut self) -> Self {
        self.expression = CronExpression::weekly();
        self
    }

    /// Run weekly on a specific day at midnight
    ///
    /// # Example
    /// ```rust,ignore
    /// .weekly_on(DayOfWeek::Monday)
    /// ```
    pub fn weekly_on(mut self, day: DayOfWeek) -> Self {
        self.expression = CronExpression::weekly_on(day);
        self
    }

    /// Run on specific days of the week at midnight
    ///
    /// # Example
    /// ```rust,ignore
    /// .days(&[DayOfWeek::Monday, DayOfWeek::Wednesday, DayOfWeek::Friday])
    /// ```
    pub fn days(mut self, days: &[DayOfWeek]) -> Self {
        self.expression = CronExpression::on_days(days);
        self
    }

    /// Run on weekdays (Monday-Friday) at midnight
    pub fn weekdays(mut self) -> Self {
        self.expression = CronExpression::weekdays();
        self
    }

    /// Run on weekends (Saturday-Sunday) at midnight
    pub fn weekends(mut self) -> Self {
        self.expression = CronExpression::weekends();
        self
    }

    /// Run on Sundays at midnight
    pub fn sundays(mut self) -> Self {
        self.expression = CronExpression::weekly_on(DayOfWeek::Sunday);
        self
    }

    /// Run on Mondays at midnight
    pub fn mondays(mut self) -> Self {
        self.expression = CronExpression::weekly_on(DayOfWeek::Monday);
        self
    }

    /// Run on Tuesdays at midnight
    pub fn tuesdays(mut self) -> Self {
        self.expression = CronExpression::weekly_on(DayOfWeek::Tuesday);
        self
    }

    /// Run on Wednesdays at midnight
    pub fn wednesdays(mut self) -> Self {
        self.expression = CronExpression::weekly_on(DayOfWeek::Wednesday);
        self
    }

    /// Run on Thursdays at midnight
    pub fn thursdays(mut self) -> Self {
        self.expression = CronExpression::weekly_on(DayOfWeek::Thursday);
        self
    }

    /// Run on Fridays at midnight
    pub fn fridays(mut self) -> Self {
        self.expression = CronExpression::weekly_on(DayOfWeek::Friday);
        self
    }

    /// Run on Saturdays at midnight
    pub fn saturdays(mut self) -> Self {
        self.expression = CronExpression::weekly_on(DayOfWeek::Saturday);
        self
    }

    /// Run once monthly on the first day at midnight
    pub fn monthly(mut self) -> Self {
        self.expression = CronExpression::monthly();
        self
    }

    /// Run monthly on a specific day at midnight
    ///
    /// # Example
    /// ```rust,ignore
    /// .monthly_on(15) // On the 15th of each month
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if `day` is outside `1..=31`. Use
    /// [`try_monthly_on`](Self::try_monthly_on) for a fallible alternative.
    /// Months without a 31st silently skip — that is cron-standard behaviour.
    pub fn monthly_on(mut self, day: u32) -> Self {
        self.expression = CronExpression::monthly_on(day);
        self
    }

    /// Fallible sibling of [`monthly_on`](Self::monthly_on): returns `Err`
    /// instead of panicking when `day` is outside `1..=31`.
    ///
    /// # Errors
    ///
    /// Returns `Err` when `day` is outside `1..=31`. Delegates to
    /// [`CronExpression::try_monthly_on`].
    pub fn try_monthly_on(mut self, day: u32) -> Result<Self, String> {
        self.expression = CronExpression::try_monthly_on(day)?;
        Ok(self)
    }

    /// Run quarterly on the first day of each quarter at midnight
    pub fn quarterly(mut self) -> Self {
        self.expression = CronExpression::quarterly();
        self
    }

    /// Run yearly on January 1st at midnight
    pub fn yearly(mut self) -> Self {
        self.expression = CronExpression::yearly();
        self
    }

    // =========================================================================
    // Configuration Methods
    // =========================================================================

    /// Set a name for this task
    ///
    /// The name is used in logs and when listing scheduled tasks.
    pub fn name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// Set a description for this task
    ///
    /// The description is shown when listing scheduled tasks.
    pub fn description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_string());
        self
    }

    /// Prevent overlapping task runs.
    ///
    /// Enforcement order:
    /// 1. [`Cache::lock`] (cross-process, fail-closed if Cache is bootstrapped
    ///    — typical production setup with a Redis or in-memory driver).
    /// 2. In-process `AtomicBool` CAS when Cache is not bootstrapped. A single
    ///    warn-once log line tells the operator they're getting the weaker
    ///    guarantee.
    ///
    /// A skipped run returns `Ok(())` (matching Laravel's silent-skip
    /// behaviour) and increments [`TaskState::skip_count`]. Configure a
    /// custom lock TTL with [`Self::without_overlapping_for`].
    ///
    /// [`Cache::lock`]: crate::cache::Cache::lock
    /// [`TaskState::skip_count`]: super::task::TaskState::skip_count
    pub fn without_overlapping(mut self) -> Self {
        self.without_overlapping = true;
        self
    }

    /// Like [`Self::without_overlapping`] but with a caller-supplied lock TTL.
    ///
    /// The TTL is the safety net for tasks that crash without releasing the
    /// lock — the next tick after this duration will see a free lock and can
    /// proceed. Pick `max(2 × expected_task_duration, 5 min)`. The default
    /// when [`Self::without_overlapping`] is used bare is 30 minutes.
    pub fn without_overlapping_for(mut self, ttl: Duration) -> Self {
        self.without_overlapping = true;
        self.overlap_ttl = Some(ttl);
        self
    }

    /// Run task in background (non-blocking)
    ///
    /// When enabled, the scheduler won't wait for the task to complete
    /// before continuing to the next task.
    pub fn run_in_background(mut self) -> Self {
        self.run_in_background = true;
        self
    }

    /// Build the task entry
    ///
    /// This is called internally when adding the task to the schedule.
    pub(crate) fn build(self, task_index: usize) -> TaskEntry {
        let name = self
            .name
            .unwrap_or_else(|| format!("closure-task-{}", task_index));

        // Zero TTL is undefined across cache backends (Redis SETEX 0 is an
        // error; in-memory treats it as instant expiration; Memcached treats
        // 0 as "never expires"). Coerce it to the documented default so all
        // backends see the same contract, and tell the operator what we did
        // so they can fix the call site.
        let overlap_ttl = match self.overlap_ttl {
            Some(d) if !d.is_zero() => d,
            Some(_zero) => {
                tracing::warn!(
                    target: "suprnova::schedule",
                    task = %name,
                    default_secs = DEFAULT_WITHOUT_OVERLAPPING_TTL.as_secs(),
                    "without_overlapping_for(Duration::ZERO) is undefined across \
                     cache backends; coerced to default",
                );
                DEFAULT_WITHOUT_OVERLAPPING_TTL
            }
            None => DEFAULT_WITHOUT_OVERLAPPING_TTL,
        };

        TaskEntry {
            name,
            expression: self.expression,
            task: self.task,
            description: self.description,
            without_overlapping: self.without_overlapping,
            run_in_background: self.run_in_background,
            overlap_ttl,
            state: TaskState::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_builder() -> TaskBuilder {
        TaskBuilder::from_async(|| async { Ok(()) })
    }

    #[test]
    fn test_builder_schedule_methods() {
        let builder = create_test_builder().every_minute();
        assert_eq!(builder.expression.expression(), "* * * * *");

        let builder = create_test_builder().hourly();
        assert_eq!(builder.expression.expression(), "0 * * * *");

        let builder = create_test_builder().daily();
        assert_eq!(builder.expression.expression(), "0 0 * * *");

        let builder = create_test_builder().weekly();
        assert_eq!(builder.expression.expression(), "0 0 * * 0");
    }

    #[test]
    fn test_builder_daily_at() {
        let builder = create_test_builder().daily_at("14:30");
        assert_eq!(builder.expression.expression(), "30 14 * * *");
    }

    #[test]
    fn test_builder_at_modifier() {
        let builder = create_test_builder().daily().at("09:15");
        assert_eq!(builder.expression.expression(), "15 9 * * *");
    }

    #[test]
    fn test_builder_configuration() {
        let builder = create_test_builder()
            .name("test-task")
            .description("A test task")
            .without_overlapping()
            .run_in_background();

        assert_eq!(builder.name, Some("test-task".to_string()));
        assert_eq!(builder.description, Some("A test task".to_string()));
        assert!(builder.without_overlapping);
        assert!(builder.run_in_background);
    }

    #[test]
    fn test_builder_build() {
        let builder = create_test_builder()
            .daily()
            .name("my-task")
            .description("My task description");

        let entry = builder.build(0);

        assert_eq!(entry.name, "my-task");
        assert_eq!(entry.description, Some("My task description".to_string()));
    }

    #[test]
    fn test_builder_default_name() {
        let builder = create_test_builder().daily();
        let entry = builder.build(5);

        assert_eq!(entry.name, "closure-task-5");
    }

    /// `Duration::ZERO` is undefined across cache backends — Redis errors,
    /// in-memory expires immediately, Memcached treats 0 as "never expire".
    /// The builder must coerce it to the documented default so every backend
    /// sees the same contract.
    #[test]
    fn without_overlapping_for_zero_duration_coerces_to_default() {
        let entry = create_test_builder()
            .every_minute()
            .name("zero-ttl")
            .without_overlapping_for(Duration::from_secs(0))
            .build(0);

        assert!(
            entry.without_overlapping,
            "the flag is still set — only the TTL is rewritten",
        );
        assert_eq!(
            entry.overlap_ttl, DEFAULT_WITHOUT_OVERLAPPING_TTL,
            "zero TTL must be coerced to the documented default",
        );
    }

    // ---- fallible schedule helpers (range validation) ------------------

    #[test]
    fn try_hourly_at_validates_minute() {
        assert!(create_test_builder().try_hourly_at(30).is_ok());
        assert!(create_test_builder().try_hourly_at(99).is_err());
    }

    #[test]
    fn try_daily_at_validates_time() {
        assert!(create_test_builder().try_daily_at("09:30").is_ok());
        assert!(create_test_builder().try_daily_at("25:00").is_err());
    }

    #[test]
    fn try_twice_daily_validates_hours() {
        let Ok(ok) = create_test_builder().try_twice_daily(1, 13) else {
            panic!("try_twice_daily(1, 13) must be Ok");
        };
        assert_eq!(ok.expression.expression(), "0 1,13 * * *");

        let Err(err) = create_test_builder().try_twice_daily(1, 99) else {
            panic!("try_twice_daily(1, 99) must be Err");
        };
        assert!(err.contains("0..=23"), "got: {err}");
    }

    #[test]
    fn try_monthly_on_validates_day() {
        assert!(create_test_builder().try_monthly_on(15).is_ok());
        assert!(create_test_builder().try_monthly_on(99).is_err());
    }

    #[test]
    fn twice_daily_still_panics_on_out_of_range() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let result = catch_unwind(AssertUnwindSafe(|| {
            create_test_builder().twice_daily(1, 99)
        }));
        assert!(
            result.is_err(),
            "infallible twice_daily must still panic on out-of-range hour",
        );
    }
}
