//! Task Scheduler Registration
//!
//! Register your scheduled tasks here. Tasks can be defined as:
//! - Struct implementing `Task` trait (recommended for complex tasks)
//! - Inline closures with fluent schedule configuration (quick tasks)
//!
//! # Examples
//!
//! ## Trait-Based Task
//!
//! ```rust,ignore
//! // In src/tasks/cleanup_logs.rs
//! use suprnova::{Task, TaskResult};
//! use async_trait::async_trait;
//!
//! pub struct CleanupLogsTask;
//!
//! #[async_trait]
//! impl Task for CleanupLogsTask {
//!     async fn handle(&self) -> TaskResult {
//!         // Your task logic
//!         Ok(())
//!     }
//! }
//!
//! // Register in schedule.rs
//! schedule.add(
//!     schedule.task(CleanupLogsTask::new())
//!         .daily()
//!         .at("03:00")
//!         .name("cleanup:logs")
//!         .description("Cleans old log files daily")
//! );
//! ```
//!
//! ## Closure-Based Task
//!
//! ```rust,ignore
//! schedule.add(
//!     schedule.call(|| async {
//!         println!("Running hourly!");
//!         Ok(())
//!     }).hourly().name("hourly-ping")
//! );
//! ```
//!
//! # Running Tasks
//!
//! ```bash
//! # Run due tasks once (for cron)
//! suprnova schedule:run
//!
//! # Run as daemon (checks every minute)
//! suprnova schedule:work
//!
//! # List all tasks
//! suprnova schedule:list
//! ```

use suprnova::Schedule;

// Import your tasks here
// use crate::tasks;

/// Register all scheduled tasks
///
/// Called by the schedule binary when starting the scheduler.
pub fn register(schedule: &mut Schedule) {
    // Example: Register a trait-based task
    // schedule.add(
    //     schedule.task(tasks::CleanupLogsTask::new())
    //         .daily()
    //         .at("03:00")
    //         .name("cleanup:logs")
    //         .description("Cleans old log files daily")
    // );

    // Example: Register a closure-based task
    // schedule.add(
    //     schedule.call(|| async {
    //         println!("Hello from scheduler!");
    //         Ok(())
    //     }).daily().at("03:00").name("daily-greeting")
    // );
}
