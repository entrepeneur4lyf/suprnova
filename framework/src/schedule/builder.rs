//! Task builder for fluent schedule configuration
//!
//! Provides a fluent API for configuring scheduled tasks with closures.

use super::expression::{CronExpression, DayOfWeek};
use super::task::{BoxedFuture, BoxedTask, ClosureTask, Task, TaskEntry, TaskResult};
use std::sync::Arc;

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
    /// Panics if the cron expression is invalid.
    pub fn cron(mut self, expression: &str) -> Self {
        self.expression = CronExpression::parse(expression).expect("Invalid cron expression");
        self
    }

    /// Try to set a custom cron expression, returning an error if invalid
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
    pub fn hourly_at(mut self, minute: u32) -> Self {
        self.expression = CronExpression::hourly_at(minute);
        self
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
    pub fn daily_at(mut self, time: &str) -> Self {
        self.expression = CronExpression::daily_at(time);
        self
    }

    /// Run twice daily at specific times
    ///
    /// # Example
    /// ```rust,ignore
    /// .twice_daily(1, 13) // At 1:00 AM and 1:00 PM
    /// ```
    pub fn twice_daily(mut self, first_hour: u32, second_hour: u32) -> Self {
        self.expression =
            CronExpression::parse(&format!("0 {},{} * * *", first_hour, second_hour)).unwrap();
        self
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
    pub fn monthly_on(mut self, day: u32) -> Self {
        self.expression = CronExpression::monthly_on(day);
        self
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

    /// Prevent overlapping task runs
    ///
    /// When enabled, the scheduler will skip running this task if
    /// a previous run is still in progress.
    pub fn without_overlapping(mut self) -> Self {
        self.without_overlapping = true;
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

        TaskEntry {
            name,
            expression: self.expression,
            task: self.task,
            description: self.description,
            without_overlapping: self.without_overlapping,
            run_in_background: self.run_in_background,
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
}
