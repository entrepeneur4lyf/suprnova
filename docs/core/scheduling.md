---
title: 'Task Scheduling'
description: 'Schedule recurring tasks with suprnova Laravel-like scheduler'
icon: 'clock'
---

suprnova provides a powerful task scheduling system inspired by Laravel's scheduler. Schedule tasks to run at specific intervals - every minute, hourly, daily, weekly, or using custom cron expressions.

## Generating Tasks

The fastest way to create a new scheduled task is using the suprnova CLI:

```bash
suprnova make:task CleanupLogs
```

This command will:
1. Create `src/tasks/cleanup_logs_task.rs` with a working task stub
2. Create `src/tasks/mod.rs` if it doesn't exist, re-exporting the task
3. Create `src/schedule.rs` for registering tasks, if it doesn't exist
4. Declare `pub mod schedule;` and `pub mod tasks;` in `src/lib.rs`
5. Wire `.schedule(<crate>::schedule::register)` into your application
   builder in `cmd/main.rs` (or `src/main.rs` for the API starter)

Steps 2–5 are idempotent, so re-running `make:task` repairs wiring that was
removed by hand. The scheduler runs inside your application binary — there is
no separate scheduler executable to build or deploy.

```bash Examples
# Creates CleanupLogsTask in src/tasks/cleanup_logs_task.rs
suprnova make:task CleanupLogs

# Creates SendRemindersTask in src/tasks/send_reminders_task.rs
suprnova make:task SendReminders

# You can also include "Task" suffix (same result)
suprnova make:task BackupDatabaseTask
```

```rust Generated File
//! CleanupLogsTask scheduled task
//!
//! Created with `suprnova make:task cleanup_logs_task`.

use std::time::Instant;

use async_trait::async_trait;
use suprnova::{Task, TaskResult};

/// CleanupLogsTask - A scheduled task.
///
/// Register the task in `src/schedule.rs` with the fluent API; the skeleton
/// below times its own run and prints a structured log line on each
/// invocation so it works end-to-end the first time you wire it up.
pub struct CleanupLogsTask;

impl CleanupLogsTask {
    /// Create a new instance of this task.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CleanupLogsTask {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Task for CleanupLogsTask {
    async fn handle(&self) -> TaskResult {
        let started_at = Instant::now();
        println!("[CleanupLogsTask] task started");

        // Replace this with the real job. The skeleton ships as a
        // no-op success so the task can be scheduled and observed
        // before the implementation is filled in.

        println!(
            "[CleanupLogsTask] task finished in {} ms",
            started_at.elapsed().as_millis(),
        );
        Ok(())
    }
}
```

## Defining Schedules

suprnova supports two approaches for defining scheduled tasks:

### 1. Trait-Based Tasks (Recommended)

For complex tasks that need dependencies or reusable logic, implement the `Task` trait and configure the schedule during registration:

```rust
// src/tasks/cleanup_logs_task.rs
use async_trait::async_trait;
use suprnova::{Task, TaskResult, DB};
use crate::models::Log;

pub struct CleanupLogsTask;

impl CleanupLogsTask {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Task for CleanupLogsTask {
    async fn handle(&self) -> TaskResult {
        // Access the database just like in controllers
        let db = DB::connection();

        // Delete logs older than 30 days
        Log::query()
            .filter(Log::created_at.lt(thirty_days_ago()))
            .delete()
            .await?;

        println!("Old logs cleaned up successfully");
        Ok(())
    }
}
```

Then register with fluent scheduling API in `src/schedule.rs`:

```rust
// src/schedule.rs
use suprnova::Schedule;
use crate::tasks::CleanupLogsTask;

pub fn register(schedule: &mut Schedule) {
    schedule.add(
        schedule.task(CleanupLogsTask::new())
            .daily()
            .at("03:00")
            .name("cleanup:logs")
            .description("Removes logs older than 30 days")
    );
}
```

### 2. Closure-Based Tasks

For quick, inline tasks without separate files:

```rust
// src/schedule.rs
use suprnova::Schedule;

pub fn register(schedule: &mut Schedule) {
    // Simple closure task
    schedule.add(
        schedule.call(|| async {
            println!("Ping! Running every minute");
            Ok(())
        })
        .every_minute()
        .name("heartbeat")
    );

    // Configured closure task
    schedule.add(
        schedule.call(|| async {
            // Your task logic
            Ok(())
        })
        .daily()
        .at("09:00")
        .name("morning-report")
        .description("Sends daily morning report")
    );
}
```

## Registering Tasks

Register your tasks in `src/schedule.rs`:

```rust
// src/schedule.rs
use suprnova::Schedule;
use crate::tasks;

pub fn register(schedule: &mut Schedule) {
    // Trait-based tasks with fluent schedule configuration
    schedule.add(
        schedule.task(tasks::CleanupLogsTask::new())
            .daily()
            .at("03:00")
            .name("cleanup:logs")
            .description("Removes logs older than 30 days")
    );

    schedule.add(
        schedule.task(tasks::SendRemindersTask::new())
            .daily()
            .at("09:00")
            .name("send:reminders")
            .description("Sends daily reminder emails")
    );

    schedule.add(
        schedule.task(tasks::BackupDatabaseTask::new())
            .weekly()
            .at("00:00")
            .name("backup:database")
            .description("Weekly database backup")
            .without_overlapping()
    );

    // Closure-based tasks
    schedule.add(
        schedule.call(|| async {
            println!("Quick task!");
            Ok(())
        })
        .hourly()
        .name("quick-task")
    );
}
```

## Schedule Frequency Options

suprnova provides a fluent API for defining when tasks should run:

### Common Intervals

| Method | Description |
|--------|-------------|
| `.every_minute()` | Run every minute |
| `.every_five_minutes()` | Run every 5 minutes |
| `.every_ten_minutes()` | Run every 10 minutes |
| `.every_fifteen_minutes()` | Run every 15 minutes |
| `.every_thirty_minutes()` | Run every 30 minutes |
| `.hourly()` | Run every hour at minute 0 |
| `.hourly_at(30)` | Run every hour at minute 30 |
| `.daily()` | Run daily at midnight |
| `.daily_at("03:00")` | Run daily at 3:00 AM |
| `.weekly()` | Run weekly on Sunday at midnight |
| `.monthly()` | Run monthly on the 1st at midnight |

### Day-Specific Schedules

```rust
use suprnova::DayOfWeek;

// Run on specific days
.weekly_on(DayOfWeek::Monday)
.weekly_on(DayOfWeek::Friday)

// Shorthand day methods
.sundays()
.mondays()
.tuesdays()
.wednesdays()
.thursdays()
.fridays()
.saturdays()

// Multiple days
.days(&[DayOfWeek::Monday, DayOfWeek::Wednesday, DayOfWeek::Friday])

// Weekdays/Weekends
.weekdays()  // Monday-Friday
.weekends()  // Saturday-Sunday
```

### Time Modifiers

Chain `.at()` with any schedule to set a specific time:

```rust
.daily().at("14:30")           // Daily at 2:30 PM
.weekly().at("09:00")          // Weekly at 9:00 AM
.mondays().at("08:00")         // Every Monday at 8:00 AM
.monthly().at("00:00")         // First of month at midnight
```

### Custom Cron Expressions

For full control, use cron syntax:

```rust
// Standard cron format: minute hour day-of-month month day-of-week
.cron("0 */2 * * *")    // Every 2 hours
.cron("30 4 * * 1-5")   // 4:30 AM on weekdays
.cron("0 0 1,15 * *")   // 1st and 15th of each month
```

## Task Configuration

### Preventing Overlapping

Skip a tick when a previous run of the same task is still in flight:

```rust
schedule.add(
    schedule.task(LongRunningTask::new())
        .daily()
        .name("long-task")
        .without_overlapping()
);
```

**How the lock works.** When the flag is set, suprnova tries to acquire a
distributed mutex via the configured [`Cache`](/docs/core/cache) backend
(`schedule:lock:<task-name>`). A successful acquire runs the task and releases
the lock; a contended acquire is reported as a successful skip — `Ok(())`,
with the task's skip counter ticked so observability surfaces can see it
without poisoning the `schedule:run` exit code.

**Cache is required for cross-process protection.** If you run multiple
processes that schedule the same task (e.g. several boxes invoking
`suprnova schedule:run` from system cron, or `schedule:work` daemons behind a
load-balancer), the Cache backend is what coordinates them. **Without a
configured Cache, `without_overlapping()` silently degrades to a per-process
`AtomicBool`** — two separate processes will not see each other's locks. The
framework emits a one-time `WARN` (`suprnova::schedule`) the first time this
fallback fires so operators notice the weaker guarantee:

> `without_overlapping() falling back to in-process AtomicBool protection — Cache is not bootstrapped. Multi-process deployments will NOT see each other's locks. Configure Cache (CACHE_DRIVER=memory|redis) before relying on cross-process overlap protection.`

**Custom lock TTL.** The lock TTL defaults to 30 minutes — long enough for
most tasks to finish, short enough that a crashed task holding the lock
unblocks the next tick without operator intervention. Override per task with
`.without_overlapping_for(Duration)`:

```rust
use std::time::Duration;

schedule.add(
    schedule.task(SlowBackupTask::new())
        .daily()
        .name("backup:full")
        // This job legitimately runs longer than the 30-minute default;
        // give the lock a 2-hour TTL so a slow run doesn't get pre-empted
        // by the next tick.
        .without_overlapping_for(Duration::from_secs(2 * 3600))
);
```

### Running in Background

Detach tasks from the per-tick critical path so they don't block other due
tasks from starting:

```rust
schedule.add(
    schedule.task(BackgroundTask::new())
        .hourly()
        .name("background-task")
        .run_in_background()
);
```

**Panic isolation.** Background tasks run inside a `tokio::task::JoinSet`
with `catch_unwind`, so a panicking task surfaces as a `FrameworkError`
recorded against the task's name rather than tearing down the scheduler. The
`schedule:work` daemon drains the JoinSet on shutdown (Ctrl-C / SIGTERM) so
in-flight background tasks complete before exit.

**Combine with `without_overlapping`.** The two flags compose — a background
task with `without_overlapping()` will spawn into the JoinSet and acquire the
overlap lock from inside the spawned future, so the lock semantics described
above still apply.

### Same-Minute Dedup

Cron resolution is minute-level, and suprnova enforces that: if the same task
is asked to run twice within the same wall-clock minute inside a single
process, the second call is a no-op skip — `Ok(())`, with the task's skip
counter ticked. This closes a class of bug where a daemon loop or a tight
`schedule:run` invocation could run a `.every_minute()` task multiple times
in the same minute.

This in-process gate is **always on**, independent of `without_overlapping`.
It does NOT span processes (each process has its own per-task state). If you
need cross-process same-minute coordination, layer on `without_overlapping`
+ a configured Cache backend — together they cover both directions.

## Running the Scheduler

suprnova provides CLI commands for running scheduled tasks:

### Run Once

Execute all due tasks once (typically called by cron every minute):

```bash
suprnova schedule:run
```

### Daemon Mode

Run continuously, checking for due tasks every minute:

```bash
suprnova schedule:work
```

This is ideal for development or when using a process manager like systemd.

### List Tasks

Display all registered scheduled tasks:

```bash
suprnova schedule:list
```

Output:
```
Registered scheduled tasks:
  cleanup:logs [0 3 * * *] — Removes logs older than 30 days
  send:reminders [0 9 * * *] — Sends daily reminder emails
  backup:database [0 0 * * 0] — Weekly database backup
```

## Production Setup

### Using Cron

Add a single cron entry to run the scheduler every minute:

```bash
* * * * * cd /path/to/your/project && suprnova schedule:run >> /dev/null 2>&1
```

**Cross-process coordination.** If you run `schedule:run` from system cron on
more than one host (or alongside a `schedule:work` daemon), tasks with
`.without_overlapping()` need a configured **Cache** backend
(`CACHE_DRIVER=redis` recommended for production) to coordinate across
processes. Without it, the overlap flag degrades to per-process protection
and the same task can run on multiple hosts in the same minute. See
[Preventing Overlapping](#preventing-overlapping) above for the full lock
semantics.

### Using Systemd

Create a systemd service for the scheduler daemon:

```ini
# /etc/systemd/system/myapp-scheduler.service
[Unit]
Description=MyApp Scheduler
After=network.target

[Service]
Type=simple
User=www-data
WorkingDirectory=/path/to/your/project
ExecStart=/path/to/suprnova schedule:work
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable myapp-scheduler
sudo systemctl start myapp-scheduler
```

## Accessing App Context

Scheduled tasks have full access to the application context, just like controllers:

```rust
use suprnova::{App, Task, TaskResult, DB};
use crate::actions::SendEmailAction;
use crate::models::User;

pub struct SendRemindersTask;

#[async_trait]
impl Task for SendRemindersTask {
    async fn handle(&self) -> TaskResult {
        // Access database
        let users = User::query()
            .filter(User::reminder_enabled.eq(true))
            .all()
            .await?;

        // Use actions via dependency injection
        let send_email: SendEmailAction = App::get().unwrap();

        for user in users {
            send_email.execute(&user.email, "Daily Reminder").await?;
        }

        Ok(())
    }
}
```

## File Organization

The recommended file structure for scheduled tasks:

```
src/
├── tasks/
│   ├── mod.rs              # Re-exports all tasks (auto-updated by make:task)
│   ├── cleanup_logs_task.rs
│   ├── send_reminders_task.rs
│   └── backup_database_task.rs
├── schedule.rs             # Registers tasks (run by the schedule:* commands)
├── bootstrap.rs
├── routes.rs
└── lib.rs                  # Declares `pub mod schedule;` + `pub mod tasks;`
cmd/
└── main.rs                 # Calls `.schedule(<crate>::schedule::register)`
```

**src/tasks/mod.rs:**
```rust
pub mod cleanup_logs_task;
pub mod send_reminders_task;
pub mod backup_database_task;

pub use cleanup_logs_task::CleanupLogsTask;
pub use send_reminders_task::SendRemindersTask;
pub use backup_database_task::BackupDatabaseTask;
```

## Summary

| Feature | Usage |
|---------|-------|
| Create task | `suprnova make:task TaskName` |
| Trait-based | Implement `Task` trait, configure schedule during registration |
| Closure-based | `schedule.call(\|\| async { ... })` |
| Register tasks | `schedule.add(schedule.task(...).daily().name("..."))` |
| Run once | `suprnova schedule:run` |
| Run daemon | `suprnova schedule:work` |
| List tasks | `suprnova schedule:list` |
| Prevent overlap | `.without_overlapping()` (default 30-min lock TTL via Cache backend) |
| Custom overlap TTL | `.without_overlapping_for(Duration)` |
| Background | `.run_in_background()` (panic-isolated via JoinSet) |
| Same-minute dedup | Always on per-process; skipped runs return `Ok(())` |
