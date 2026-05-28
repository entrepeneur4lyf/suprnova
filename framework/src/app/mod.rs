//! Application builder for suprnova framework
//!
//! Provides a fluent builder API to configure and run a suprnova application.
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::Application;
//!
//! #[tokio::main]
//! async fn main() {
//!     Application::new()
//!         .config(config::register_all)
//!         .bootstrap(bootstrap::register)
//!         .routes(routes::register)
//!         .migrations::<migrations::Migrator>()
//!         .run()
//!         .await;
//! }
//! ```

use crate::{Config, Router, Schedule, Server};
use clap::{Parser, Subcommand};
use sea_orm_migration::prelude::*;
use std::env;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

pub mod maintenance;
pub mod paths;

/// Boxed async bootstrap function (avoids repeating the complex trait-object type).
type BootstrapFn = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// Boxed callback run once after the server boots its services.
type BootedFn = Box<dyn FnOnce()>;

/// Boxed function that registers the application's scheduled tasks.
type ScheduleFn = Box<dyn FnOnce(&mut Schedule) + Send>;

/// CLI structure for suprnova applications
#[derive(Parser)]
#[command(name = "app")]
#[command(about = "suprnova application server and utilities")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the web server (default command)
    Serve {
        /// Skip running migrations on startup
        #[arg(long)]
        no_migrate: bool,
    },
    /// Run the web server (alias for serve)
    #[command(name = "web:run")]
    WebRun {
        /// Skip running migrations on startup
        #[arg(long)]
        no_migrate: bool,
    },
    /// Run pending database migrations
    Migrate,
    /// Show migration status
    #[command(name = "migrate:status")]
    MigrateStatus,
    /// Rollback the last migration(s)
    #[command(name = "migrate:rollback")]
    MigrateRollback {
        /// Number of migrations to rollback
        #[arg(default_value = "1")]
        steps: u32,
    },
    /// Drop all tables and re-run all migrations
    #[command(name = "migrate:fresh")]
    MigrateFresh,
    /// Run the scheduler daemon (checks every minute)
    #[command(name = "schedule:work")]
    ScheduleWork,
    /// Run all due scheduled tasks once
    #[command(name = "schedule:run")]
    ScheduleRun,
    /// List all registered scheduled tasks
    #[command(name = "schedule:list")]
    ScheduleList,
    /// Run the workflow worker daemon
    #[command(name = "workflow:work")]
    WorkflowWork,
    /// Run the queue worker daemon (drains the configured queue driver)
    #[command(name = "queue:work")]
    QueueWork {
        /// Visibility timeout for popped messages (seconds). Drivers may
        /// interpret this differently; see driver docs.
        #[arg(long, default_value = "60")]
        visibility_timeout: u64,
        /// Poll interval when the queue is empty (milliseconds).
        #[arg(long = "poll", default_value = "100")]
        poll_interval_ms: u64,
        /// Exit cleanly after processing this many jobs. Useful for
        /// release-on-restart deploys (worker exits, supervisor restarts).
        #[arg(long)]
        max_jobs: Option<u64>,
    },
    /// Put the application into maintenance mode
    Down {
        /// Seconds for the `Retry-After` header
        #[arg(long)]
        retry: Option<u64>,
        /// Seconds for the browser `Refresh` header
        #[arg(long)]
        refresh: Option<u64>,
        /// Secret URL segment that bypasses maintenance mode
        #[arg(long)]
        secret: Option<String>,
        /// Generate a random bypass secret and print it
        #[arg(long = "with-secret")]
        with_secret: bool,
        /// Redirect visitors to this path instead of serving the 503
        #[arg(long)]
        redirect: Option<String>,
        /// HTTP status code for the maintenance response
        #[arg(long, default_value = "503")]
        status: u16,
        /// A path that stays reachable while down (repeatable)
        #[arg(long = "except")]
        except: Vec<String>,
        /// Plain-text message rendered in the maintenance response body
        #[arg(long)]
        message: Option<String>,
    },
    /// Bring the application out of maintenance mode
    Up,
}

/// Application builder for suprnova framework
///
/// Use this to configure and run your suprnova application with a fluent API.
pub struct Application<M = NoMigrator>
where
    M: MigratorTrait,
{
    config_fn: Option<Box<dyn FnOnce()>>,
    bootstrap_fn: Option<BootstrapFn>,
    routes_fn: Option<Box<dyn FnOnce() -> Router + Send>>,
    schedule_fn: Option<ScheduleFn>,
    booted_fns: Vec<BootedFn>,
    _migrator: std::marker::PhantomData<M>,
}

/// Placeholder type for when no migrator is configured
pub struct NoMigrator;

impl MigratorTrait for NoMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![]
    }
}

impl Application<NoMigrator> {
    /// Create a new application builder
    pub fn new() -> Self {
        Application {
            config_fn: None,
            bootstrap_fn: None,
            routes_fn: None,
            schedule_fn: None,
            booted_fns: Vec::new(),
            _migrator: std::marker::PhantomData,
        }
    }

    /// The Suprnova framework version this application is built against
    /// (the `suprnova` crate's version).
    pub fn framework_version() -> &'static str {
        crate::VERSION
    }
}

impl Default for Application<NoMigrator> {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a [`Schedule`] by running the registered `schedule_fn` (if any) against
/// a fresh schedule.
///
/// Extracted as a free function (not a method on `Application<M>`) so unit tests
/// can drive the schedule registration flow without instantiating an
/// `Application<NoMigrator>` and without the migrator type bleeding into test
/// expectations.
pub(crate) fn build_schedule(schedule_fn: Option<ScheduleFn>) -> Schedule {
    let mut schedule = Schedule::new();
    if let Some(f) = schedule_fn {
        f(&mut schedule);
    }
    schedule
}

/// Render the `schedule:list` output for a built [`Schedule`].
///
/// Returns the exact string the handler would print to stdout, so callers can
/// either `print!("{}", …)` from a CLI handler or assert on it from a test
/// without capturing stdout. Trailing newline is included so the caller does
/// not have to worry about whether the schedule is empty.
pub(crate) fn format_schedule_listing(schedule: &Schedule) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if schedule.is_empty() {
        out.push_str("No scheduled tasks registered.\n");
        out.push_str(
            "Define tasks in src/schedule.rs and wire it with \
             `Application::schedule(schedule::register)`.\n",
        );
        return out;
    }
    out.push_str("Registered scheduled tasks:\n");
    for entry in schedule.tasks() {
        let expr = entry.expression.expression();
        let _ = match &entry.description {
            Some(desc) => writeln!(out, "  {} [{expr}] — {desc}", entry.name),
            None => writeln!(out, "  {} [{expr}]", entry.name),
        };
    }
    out
}

/// Evaluate every currently-due task in the schedule and collect the results.
///
/// Returns `(results, any_failed)` so the CLI handler can drive the success/
/// failure exit semantics while tests can assert on the structured outcome
/// without intercepting `std::process::exit`.
pub(crate) async fn evaluate_due_once(
    schedule: &Schedule,
) -> (
    Vec<(String, Result<(), crate::error::FrameworkError>)>,
    bool,
) {
    let results: Vec<(String, Result<(), crate::error::FrameworkError>)> = schedule
        .run_due_tasks()
        .await
        .into_iter()
        .map(|(name, result)| (name.to_owned(), result))
        .collect();
    let any_failed = results.iter().any(|(_, r)| r.is_err());
    (results, any_failed)
}

impl<M> Application<M>
where
    M: MigratorTrait,
{
    /// Register a configuration function
    ///
    /// This function is called early during startup to register
    /// application configuration.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// App::new()
    ///     .config(config::register_all)
    /// ```
    pub fn config<F>(mut self, f: F) -> Self
    where
        F: FnOnce() + 'static,
    {
        self.config_fn = Some(Box::new(f));
        self
    }

    /// Register a bootstrap function
    ///
    /// This async function is called to register services, middleware,
    /// and other application components.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// App::new()
    ///     .bootstrap(bootstrap::register)
    /// ```
    pub fn bootstrap<F, Fut>(mut self, f: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.bootstrap_fn = Some(Box::new(move || Box::pin(f())));
        self
    }

    /// Register a routes function
    ///
    /// This function returns the application's router configuration.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// App::new()
    ///     .routes(routes::register)
    /// ```
    pub fn routes<F>(mut self, f: F) -> Self
    where
        F: FnOnce() -> Router + Send + 'static,
    {
        self.routes_fn = Some(Box::new(f));
        self
    }

    /// Register a callback to run once after the server has booted its
    /// services (i.e. after `Server::from_config` has run service
    /// registration), and before it begins accepting connections.
    ///
    /// Unlike [`bootstrap`](Self::bootstrap), which registers services, a
    /// `booted` callback can *resolve* them from the container.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// App::new()
    ///     .booted(|| {
    ///         let cfg: MyConfig = App::get().unwrap();
    ///         tracing::info!(?cfg, "services booted");
    ///     })
    /// ```
    pub fn booted<F>(mut self, f: F) -> Self
    where
        F: FnOnce() + 'static,
    {
        self.booted_fns.push(Box::new(f));
        self
    }

    /// Register the application's scheduled tasks.
    ///
    /// The function receives a mutable [`Schedule`] to add tasks to; it is run
    /// by the `schedule:work` (daemon), `schedule:run` (run-due-once), and
    /// `schedule:list` subcommands. Without it, those commands report that no
    /// tasks are registered.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Application::new()
    ///     .schedule(schedule::register)
    /// ```
    pub fn schedule<F>(mut self, f: F) -> Self
    where
        F: FnOnce(&mut Schedule) + Send + 'static,
    {
        self.schedule_fn = Some(Box::new(f));
        self
    }

    /// Configure the migrator type for database migrations
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// Application::new()
    ///     .migrations::<migrations::Migrator>()
    /// ```
    pub fn migrations<NewM>(self) -> Application<NewM>
    where
        NewM: MigratorTrait,
    {
        Application {
            config_fn: self.config_fn,
            bootstrap_fn: self.bootstrap_fn,
            routes_fn: self.routes_fn,
            schedule_fn: self.schedule_fn,
            booted_fns: self.booted_fns,
            _migrator: std::marker::PhantomData,
        }
    }

    /// Run the application
    ///
    /// This parses CLI arguments and executes the appropriate command:
    /// - `serve` (default): Run the web server
    /// - `web:run`: Run the web server (alias for serve)
    /// - `migrate`: Run pending migrations
    /// - `migrate:status`: Show migration status
    /// - `migrate:rollback`: Rollback migrations
    /// - `migrate:fresh`: Drop and re-run all migrations
    /// - `schedule:*`: Scheduler commands
    /// - `down` / `up`: Enter / leave maintenance mode
    pub async fn run(self) {
        let cli = Cli::parse();

        // Initialize framework configuration (loads .env files)
        Config::init(Path::new("."));

        // Register all #[policy] gates collected via inventory::submit!.
        // Called here (before the subcommand match) so background workers,
        // CLI commands, and scheduled tasks all see registered gates — not
        // only the web server path. The inner `Once` makes this idempotent.
        crate::authorization::init_policies();

        // Destructure self to avoid partial move issues
        let Application {
            config_fn,
            bootstrap_fn,
            routes_fn,
            schedule_fn,
            booted_fns,
            _migrator,
        } = self;

        // Run user's config registration
        if let Some(config_fn) = config_fn {
            config_fn();
        }

        match cli.command {
            None
            | Some(Commands::Serve { no_migrate: false })
            | Some(Commands::WebRun { no_migrate: false }) => {
                // Default: run server with auto-migrate
                Self::run_migrations_silent::<M>().await;
                Self::run_server_internal(bootstrap_fn, routes_fn, booted_fns).await;
            }
            Some(Commands::Serve { no_migrate: true })
            | Some(Commands::WebRun { no_migrate: true }) => {
                // Run server without migrations
                Self::run_server_internal(bootstrap_fn, routes_fn, booted_fns).await;
            }
            Some(Commands::Migrate) => {
                Self::run_migrations::<M>().await;
            }
            Some(Commands::MigrateStatus) => {
                Self::show_migration_status::<M>().await;
            }
            Some(Commands::MigrateRollback { steps }) => {
                Self::rollback_migrations::<M>(steps).await;
            }
            Some(Commands::MigrateFresh) => {
                Self::fresh_migrations::<M>().await;
            }
            Some(Commands::ScheduleWork) => {
                Self::run_scheduler_daemon_internal(bootstrap_fn, schedule_fn).await;
            }
            Some(Commands::ScheduleRun) => {
                Self::run_scheduled_tasks_internal(bootstrap_fn, schedule_fn).await;
            }
            Some(Commands::ScheduleList) => {
                Self::list_scheduled_tasks(schedule_fn).await;
            }
            Some(Commands::WorkflowWork) => {
                Self::run_workflow_worker_internal(bootstrap_fn).await;
            }
            Some(Commands::QueueWork {
                visibility_timeout,
                poll_interval_ms,
                max_jobs,
            }) => {
                Self::run_queue_worker_internal(
                    bootstrap_fn,
                    visibility_timeout,
                    poll_interval_ms,
                    max_jobs,
                )
                .await;
            }
            Some(Commands::Down {
                retry,
                refresh,
                secret,
                with_secret,
                redirect,
                status,
                except,
                message,
            }) => {
                Self::run_down(
                    retry,
                    refresh,
                    secret,
                    with_secret,
                    redirect,
                    status,
                    except,
                    message,
                )
                .await;
            }
            Some(Commands::Up) => {
                Self::run_up().await;
            }
        }
    }

    async fn run_server_internal(
        bootstrap_fn: Option<BootstrapFn>,
        routes_fn: Option<Box<dyn FnOnce() -> Router + Send>>,
        booted_fns: Vec<BootedFn>,
    ) {
        // Run bootstrap
        if let Some(bootstrap_fn) = bootstrap_fn {
            bootstrap_fn().await;
        }

        // Get router
        let router = if let Some(routes_fn) = routes_fn {
            routes_fn()
        } else {
            Router::new()
        };

        // Create server with configuration from environment.
        //
        // `from_config` returns Err when APP_KEY is required (any
        // non-development environment) but unset or malformed. The
        // error type carries the user-facing remediation (it points at
        // `suprnova key:generate`); we surface it on stderr without a
        // panic stack-trace wrapper so production boot logs stay clean.
        let server = match Server::from_config(router) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("suprnova: failed to start server: {e}");
                std::process::exit(1);
            }
        };

        // Services are booted now (Server::from_config ran service
        // registration); fire the registered `booted` callbacks before
        // the server begins accepting connections.
        for booted in booted_fns {
            booted();
        }

        if let Err(e) = server.run().await {
            eprintln!("suprnova: server exited with error: {e}");
            std::process::exit(1);
        }
    }

    async fn get_database_connection() -> sea_orm::DatabaseConnection {
        let database_url = env::var("DATABASE_URL").unwrap_or_else(|_| {
            eprintln!(
                "suprnova: DATABASE_URL is not set. \
                 Configure DATABASE_URL in your environment (e.g. .env) \
                 before running a database subcommand."
            );
            std::process::exit(1);
        });

        // For SQLite, ensure the database file can be created
        let database_url = if database_url.starts_with("sqlite://") {
            let path = database_url.trim_start_matches("sqlite://");
            let path = path.trim_start_matches("./");

            if let Some(parent) = Path::new(path).parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent).ok();
            }

            if !Path::new(path).exists() {
                std::fs::File::create(path).ok();
            }

            format!("sqlite:{}?mode=rwc", path)
        } else {
            database_url
        };

        sea_orm::Database::connect(&database_url)
            .await
            .unwrap_or_else(|e| {
                eprintln!("suprnova: failed to connect to the database: {e}");
                std::process::exit(1);
            })
    }

    async fn run_migrations_silent<Migrator: MigratorTrait>() {
        let db = Self::get_database_connection().await;
        if let Err(e) = Migrator::up(&db, None).await {
            eprintln!("Warning: Migration failed: {}", e);
        }
    }

    async fn run_migrations<Migrator: MigratorTrait>() {
        println!("Running migrations...");
        let db = Self::get_database_connection().await;
        if let Err(e) = Migrator::up(&db, None).await {
            eprintln!("suprnova: migration failed: {e}");
            std::process::exit(1);
        }
        println!("Migrations completed successfully!");
    }

    async fn show_migration_status<Migrator: MigratorTrait>() {
        println!("Migration status:");
        let db = Self::get_database_connection().await;
        if let Err(e) = Migrator::status(&db).await {
            eprintln!("suprnova: failed to read migration status: {e}");
            std::process::exit(1);
        }
    }

    async fn rollback_migrations<Migrator: MigratorTrait>(steps: u32) {
        println!("Rolling back {} migration(s)...", steps);
        let db = Self::get_database_connection().await;
        if let Err(e) = Migrator::down(&db, Some(steps)).await {
            eprintln!("suprnova: rollback failed: {e}");
            std::process::exit(1);
        }
        println!("Rollback completed successfully!");
    }

    async fn fresh_migrations<Migrator: MigratorTrait>() {
        println!("WARNING: Dropping all tables and re-running migrations...");
        let db = Self::get_database_connection().await;
        if let Err(e) = Migrator::fresh(&db).await {
            eprintln!("suprnova: database refresh failed: {e}");
            std::process::exit(1);
        }
        println!("Database refreshed successfully!");
    }

    /// `schedule:work`: run the scheduler as a long-lived daemon.
    ///
    /// The first tick is aligned to the next minute boundary, then due tasks
    /// are evaluated once per minute (matching Laravel's per-minute cron
    /// evaluation). Boots the runtime drivers + the app's `bootstrap_fn` first
    /// so tasks can resolve services; stops on Ctrl-C.
    async fn run_scheduler_daemon_internal(
        bootstrap_fn: Option<BootstrapFn>,
        schedule_fn: Option<ScheduleFn>,
    ) {
        if let Err(e) = Self::bootstrap_runtime_drivers().await {
            eprintln!("suprnova: scheduler bootstrap error: {e}");
            std::process::exit(1);
        }
        if let Some(bootstrap_fn) = bootstrap_fn {
            bootstrap_fn().await;
        }
        let schedule = build_schedule(schedule_fn);

        println!("==============================================");
        println!("  suprnova Scheduler Daemon");
        println!("==============================================");
        println!(
            "  {} task(s) registered. Press Ctrl+C to stop.",
            schedule.len()
        );
        println!("==============================================");

        // Align the first tick to the next minute boundary, then tick once a
        // minute. Cron expressions are evaluated against the wall clock at each
        // tick, so alignment keeps a `* * * * *` task firing at :00.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let until_next_minute = Duration::from_secs(60 - (now.as_secs() % 60))
            .saturating_sub(Duration::from_nanos(now.subsec_nanos() as u64));
        let mut tick = tokio::time::interval_at(
            tokio::time::Instant::now() + until_next_minute,
            Duration::from_secs(60),
        );
        // A task run that overruns a minute must not trigger a catch-up burst
        // that re-evaluates the same wall-clock minute (double-firing tasks);
        // skip missed ticks and resume on the next aligned boundary.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    for (name, result) in schedule.run_due_tasks().await {
                        if let Err(e) = result {
                            eprintln!("suprnova: scheduled task '{name}' failed: {e}");
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    println!("suprnova: scheduler shutting down.");
                    break;
                }
            }
        }
    }

    /// `schedule:run`: evaluate and run the due tasks once, then exit. Exits
    /// non-zero if any task failed.
    async fn run_scheduled_tasks_internal(
        bootstrap_fn: Option<BootstrapFn>,
        schedule_fn: Option<ScheduleFn>,
    ) {
        if let Err(e) = Self::bootstrap_runtime_drivers().await {
            eprintln!("suprnova: scheduler bootstrap error: {e}");
            std::process::exit(1);
        }
        if let Some(bootstrap_fn) = bootstrap_fn {
            bootstrap_fn().await;
        }
        let schedule = build_schedule(schedule_fn);

        println!("Running due scheduled tasks...");
        let (results, any_failed) = evaluate_due_once(&schedule).await;
        if results.is_empty() {
            println!("No tasks were due.");
            return;
        }
        for (name, result) in &results {
            match result {
                Ok(()) => println!("  ✓ {name}"),
                Err(e) => eprintln!("  ✗ {name}: {e}"),
            }
        }
        if any_failed {
            std::process::exit(1);
        }
    }

    /// `schedule:list`: print every registered task and its cron expression.
    async fn list_scheduled_tasks(schedule_fn: Option<ScheduleFn>) {
        let schedule = build_schedule(schedule_fn);
        print!("{}", format_schedule_listing(&schedule));
    }

    async fn run_workflow_worker_internal(bootstrap_fn: Option<BootstrapFn>) {
        if let Err(e) = Self::bootstrap_runtime_drivers().await {
            eprintln!("Workflow worker bootstrap error: {e}");
            std::process::exit(1);
        }

        if let Some(bootstrap_fn) = bootstrap_fn {
            bootstrap_fn().await;
        }

        println!("==============================================");
        println!("  suprnova Workflow Worker");
        println!("==============================================");
        println!();
        println!("  Press Ctrl+C to stop");
        println!();
        println!("==============================================");

        if let Err(e) = crate::workflow::WorkflowWorker::work_loop().await {
            eprintln!("Workflow worker error: {e}");
            std::process::exit(1);
        }
    }

    /// `queue:work`: drain the configured queue driver until cancelled.
    ///
    /// Boots the runtime drivers + the app's `bootstrap_fn` so popped jobs
    /// can resolve services from the container. Honours Ctrl-C cleanly via
    /// `CancellationToken`: the cancel fires at the next pop boundary, so an
    /// in-flight handler runs to completion (bounded by its own per-job
    /// `timeout()` if set) before the worker exits.
    async fn run_queue_worker_internal(
        bootstrap_fn: Option<BootstrapFn>,
        visibility_timeout: u64,
        poll_interval_ms: u64,
        max_jobs: Option<u64>,
    ) {
        if let Err(e) = Self::bootstrap_runtime_drivers().await {
            eprintln!("suprnova: queue worker bootstrap error: {e}");
            std::process::exit(1);
        }
        if let Some(bootstrap_fn) = bootstrap_fn {
            bootstrap_fn().await;
        }

        let driver = match crate::queue::Queue::driver() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("suprnova: no queue driver configured: {e}");
                std::process::exit(1);
            }
        };

        let cfg = crate::queue::worker::WorkerConfig {
            visibility_timeout: Duration::from_secs(visibility_timeout),
            poll_interval: Duration::from_millis(poll_interval_ms),
            max_jobs,
        };

        let cancel = tokio_util::sync::CancellationToken::new();

        println!("==============================================");
        println!("  suprnova Queue Worker");
        println!("==============================================");
        println!("  driver:             {}", driver.name());
        println!("  visibility timeout: {visibility_timeout}s");
        println!("  poll interval:      {poll_interval_ms}ms");
        if let Some(m) = max_jobs {
            println!("  max jobs:           {m} (exits after)");
        } else {
            println!("  max jobs:           unlimited");
        }
        println!("  Press Ctrl+C to stop");
        println!("==============================================");

        let cancel_for_worker = cancel.clone();
        let mut worker = tokio::spawn(async move {
            crate::queue::worker::run_worker(driver, cfg, cancel_for_worker).await;
        });

        // Either Ctrl-C fires (then we cancel and wait for in-flight to
        // settle) or the worker exits on its own (max_jobs reached).
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("suprnova: queue worker shutting down (Ctrl-C).");
                cancel.cancel();
                if let Err(e) = worker.await {
                    eprintln!("suprnova: queue worker task error during drain: {e}");
                    std::process::exit(1);
                }
            }
            res = &mut worker => {
                if let Err(e) = res {
                    eprintln!("suprnova: queue worker task error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }

    /// Shared bootstrap for non-server subcommands that still need the
    /// runtime drivers: Cache, Queue, RateLimit, Mail. Mirrors the
    /// driver-bootstrap order in `Server::run` (telemetry / encryption
    /// keys / authorization init are subcommand-specific and stay out
    /// of this helper).
    async fn bootstrap_runtime_drivers() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        crate::cache::Cache::bootstrap().await?;
        crate::queue::bootstrap_from_env().await?;
        crate::rate_limit::bootstrap_from_env().await?;
        crate::mail::boot::bootstrap_from_env()?;
        Ok(())
    }

    /// `down`: record the maintenance payload via the configured driver.
    #[allow(clippy::too_many_arguments)]
    async fn run_down(
        retry: Option<u64>,
        refresh: Option<u64>,
        secret: Option<String>,
        with_secret: bool,
        redirect: Option<String>,
        status: u16,
        except: Vec<String>,
        message: Option<String>,
    ) {
        Self::bootstrap_maintenance_driver().await;

        let secret = match (secret, with_secret) {
            (Some(s), _) => Some(s),
            (None, true) => Some(maintenance::random_secret()),
            (None, false) => None,
        };

        let payload = maintenance::MaintenancePayload {
            except,
            redirect,
            retry,
            refresh,
            secret: secret.clone(),
            status,
            template: message,
        };

        match maintenance::maintenance_mode().activate(&payload).await {
            Ok(()) => {
                println!("Application is now in maintenance mode.");
                if let Some(secret) = secret {
                    println!("Bypass maintenance mode by visiting: /{secret}");
                }
            }
            Err(e) => {
                eprintln!("suprnova: failed to enter maintenance mode: {e}");
                std::process::exit(1);
            }
        }
    }

    /// `up`: clear maintenance state via the configured driver.
    async fn run_up() {
        Self::bootstrap_maintenance_driver().await;

        match maintenance::maintenance_mode().deactivate().await {
            Ok(()) => println!("Application is now live."),
            Err(e) => {
                eprintln!("suprnova: failed to bring the application up: {e}");
                std::process::exit(1);
            }
        }
    }

    /// The cache-backed maintenance driver needs the cache bootstrapped; the
    /// file driver needs nothing. Only boot the cache when it's in use.
    async fn bootstrap_maintenance_driver() {
        if env::var("MAINTENANCE_DRIVER").as_deref() == Ok("cache")
            && let Err(e) = crate::cache::Cache::bootstrap().await
        {
            eprintln!("suprnova: maintenance (cache driver) bootstrap failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    //! End-to-end coverage for the `schedule:list` / `schedule:run` /
    //! `schedule:work` integration points. The free helpers
    //! [`build_schedule`], [`format_schedule_listing`], and
    //! [`evaluate_due_once`] are the exact code the three CLI subcommand
    //! handlers delegate to, so exercising them here proves the
    //! `Application::schedule(f)` registration flow reaches the user's
    //! `schedule_fn` and produces the same artefacts the binary would emit.
    use super::*;
    use crate::error::FrameworkError;
    use crate::schedule::{Schedule, Task, TaskResult};
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn build_schedule_with_none_returns_empty_schedule() {
        let schedule = build_schedule(None);
        assert!(
            schedule.is_empty(),
            "no schedule_fn should produce an empty Schedule",
        );
    }

    #[test]
    fn build_schedule_runs_user_fn_against_fresh_schedule() {
        let f: ScheduleFn = Box::new(|sched: &mut Schedule| {
            let b = sched.call(|| async { Ok(()) }).every_minute().name("a");
            sched.add(b);
            let b = sched.call(|| async { Ok(()) }).hourly().name("b");
            sched.add(b);
        });
        let schedule = build_schedule(Some(f));
        assert_eq!(schedule.len(), 2);
        assert!(schedule.find("a").is_some());
        assert!(schedule.find("b").is_some());
    }

    #[test]
    fn format_schedule_listing_empty_includes_registration_hint() {
        let schedule = build_schedule(None);
        let out = format_schedule_listing(&schedule);
        assert!(
            out.contains("No scheduled tasks registered."),
            "empty listing should announce no tasks: {out:?}",
        );
        assert!(
            out.contains("Application::schedule(schedule::register)"),
            "empty listing should suggest the registration call: {out:?}",
        );
    }

    #[test]
    fn format_schedule_listing_renders_name_expression_and_description() {
        let f: ScheduleFn = Box::new(|sched: &mut Schedule| {
            let b = sched
                .call(|| async { Ok(()) })
                .every_minute()
                .name("nightly-cleanup")
                .description("Remove stale upload temp files");
            sched.add(b);
            let b = sched
                .call(|| async { Ok(()) })
                .hourly()
                .name("plain-hourly");
            sched.add(b);
        });
        let schedule = build_schedule(Some(f));
        let out = format_schedule_listing(&schedule);
        assert!(out.starts_with("Registered scheduled tasks:\n"));
        assert!(out.contains("nightly-cleanup"));
        assert!(out.contains("[* * * * *]"));
        assert!(out.contains("— Remove stale upload temp files"));
        assert!(out.contains("plain-hourly"));
        assert!(out.contains("[0 * * * *]"));
    }

    /// `evaluate_due_once` is what `schedule:run` delegates to. The handler
    /// uses the returned `any_failed` flag to choose its process exit code; a
    /// test that asserts the flag covers the success-path contract end-to-end
    /// without spawning a child process.
    #[tokio::test]
    async fn evaluate_due_once_executes_due_tasks_and_marks_success() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);
        let f: ScheduleFn = Box::new(move |sched: &mut Schedule| {
            let counter = Arc::clone(&calls_clone);
            let b = sched
                .call(move || {
                    let counter = Arc::clone(&counter);
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                })
                .every_minute()
                .name("ok-task");
            sched.add(b);
        });
        let schedule = build_schedule(Some(f));
        let (results, any_failed) = evaluate_due_once(&schedule).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "ok-task");
        assert!(results[0].1.is_ok());
        assert!(
            !any_failed,
            "no failed tasks should report any_failed=false"
        );
    }

    #[tokio::test]
    async fn evaluate_due_once_reports_failure_via_any_failed_flag() {
        let f: ScheduleFn = Box::new(|sched: &mut Schedule| {
            let b = sched
                .call(|| async { Err(FrameworkError::internal("boom")) })
                .every_minute()
                .name("boom-task");
            sched.add(b);
            let b = sched
                .call(|| async { Ok(()) })
                .every_minute()
                .name("ok-task");
            sched.add(b);
        });
        let schedule = build_schedule(Some(f));
        let (results, any_failed) = evaluate_due_once(&schedule).await;
        assert_eq!(results.len(), 2);
        assert!(any_failed, "a failing task must flip any_failed");
        let by_name: std::collections::BTreeMap<_, _> = results
            .iter()
            .map(|(n, r)| (n.as_str(), r.is_err()))
            .collect();
        assert_eq!(by_name.get("boom-task"), Some(&true));
        assert_eq!(by_name.get("ok-task"), Some(&false));
    }

    #[tokio::test]
    async fn evaluate_due_once_with_empty_schedule_returns_empty_results() {
        let schedule = build_schedule(None);
        let (results, any_failed) = evaluate_due_once(&schedule).await;
        assert!(results.is_empty());
        assert!(!any_failed);
    }

    /// Trait-based tasks must reach the same registration / listing /
    /// evaluation pipeline as closure-based ones — proves
    /// `Schedule::task(T)` and `Schedule::call(|| ...)` both round-trip
    /// through the CLI helpers.
    #[tokio::test]
    async fn application_pipeline_handles_trait_based_tasks() {
        struct CleanupTask {
            ran: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl Task for CleanupTask {
            async fn handle(&self) -> TaskResult {
                self.ran.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let ran = Arc::new(AtomicUsize::new(0));
        let ran_clone = Arc::clone(&ran);
        let f: ScheduleFn = Box::new(move |sched: &mut Schedule| {
            let task = CleanupTask { ran: ran_clone };
            let b = sched.task(task).every_minute().name("cleanup");
            sched.add(b);
        });
        let schedule = build_schedule(Some(f));
        let listing = format_schedule_listing(&schedule);
        assert!(listing.contains("cleanup"));
        assert!(listing.contains("[* * * * *]"));

        let (results, any_failed) = evaluate_due_once(&schedule).await;
        assert_eq!(results.len(), 1);
        assert!(!any_failed);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }
}
