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
    let results = schedule.run_due_tasks().await;
    let any_failed = results.iter().any(|(_, r)| r.is_err());
    (results, any_failed)
}

/// Environment variable that opts the default `serve` / `web:run` auto-migrate
/// path back into the legacy log-and-continue behaviour.
///
/// Unset (or set to any non-truthy value) keeps the production-safe default:
/// migration errors abort the process before the HTTP server boots. Set to
/// `true` / `1` / `yes` / `on` (case-insensitive, trimmed) to log a warning
/// and continue.
pub(crate) const AUTO_MIGRATE_BEST_EFFORT_ENV: &str = "SUPRNOVA_AUTO_MIGRATE_BEST_EFFORT";

/// Parse the truthiness of [`AUTO_MIGRATE_BEST_EFFORT_ENV`].
///
/// Accepts `true`, `1`, `yes`, `on` (case-insensitive, surrounding whitespace
/// stripped). Everything else — including `false`, `0`, empty strings, and the
/// `None` returned by [`std::env::var`] when the variable is unset — yields
/// `false` so the production-safe fail-closed path is the default.
///
/// Extracted as a pure function so the parsing contract is unit-testable
/// without mutating the process-global environment.
pub(crate) fn parse_auto_migrate_best_effort(value: Option<&str>) -> bool {
    value
        .map(|v| {
            let v = v.trim();
            v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
                || v == "1"
        })
        .unwrap_or(false)
}

/// Apply the fail-closed-by-default auto-migration policy to a `Migrator::up`
/// result.
///
/// When `best_effort` is `false` (the default), a migration error is returned
/// as-is so the caller can abort the server boot. When `best_effort` is
/// `true`, the error is logged to stderr and swallowed so the caller can
/// continue into the server.
///
/// Extracted as a pure function so the policy is unit-testable without going
/// through `std::process::exit` or spinning up a real `Application::run`.
pub(crate) fn resolve_auto_migration(
    outcome: Result<(), sea_orm::DbErr>,
    best_effort: bool,
) -> Result<(), sea_orm::DbErr> {
    match outcome {
        Ok(()) => Ok(()),
        Err(e) if best_effort => {
            eprintln!(
                "suprnova: WARNING — auto-migrate failed in best-effort mode, server will boot \
                 against the current schema: {e}"
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Format a single background-task completion for the scheduler daemon's
/// stderr log. Failures (handler `Err`, task panic, or `JoinError` from
/// cancellation) print a single line; success completions are silent so
/// per-minute heartbeats don't drown out real signal.
fn report_background_outcome(
    joined: Result<crate::schedule::ScheduledTaskJoin, tokio::task::JoinError>,
) {
    match joined {
        Ok((_name, Ok(()))) => {}
        Ok((name, Err(e))) => {
            eprintln!("suprnova: scheduled task '{name}' failed: {e}")
        }
        Err(e) => {
            eprintln!("suprnova: scheduled task join error: {e}")
        }
    }
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

    /// Auto-migrate path for the default `serve` / `web:run` arms.
    ///
    /// **Fails closed by default.** If `Migrator::up` returns an error the
    /// process aborts with `exit(1)` rather than booting the HTTP server
    /// against a partially-migrated schema. The matching no-server paths
    /// (`migrate`, `migrate:status`, `migrate:rollback`, `migrate:fresh`)
    /// already exit on error; this brings the server entry into the same
    /// contract.
    ///
    /// Operators who deliberately want the old log-and-continue behaviour
    /// can opt in by setting [`AUTO_MIGRATE_BEST_EFFORT_ENV`] to one of the
    /// truthy values accepted by [`parse_auto_migrate_best_effort`]. The
    /// process then logs a warning and continues into the server boot.
    async fn run_migrations_silent<Migrator: MigratorTrait>() {
        // If the configured migrator has no migrations (the default
        // `NoMigrator`, or any app-defined migrator with an empty set),
        // skip the database connection entirely. This is the default
        // `serve`/`web:run` arm, and a framework app without a database
        // should boot successfully without `DATABASE_URL` being set.
        // Explicit subcommands like `migrate` continue to require it.
        if Migrator::migrations().is_empty() {
            return;
        }
        let best_effort =
            parse_auto_migrate_best_effort(env::var(AUTO_MIGRATE_BEST_EFFORT_ENV).ok().as_deref());
        let db = Self::get_database_connection().await;
        let outcome = Migrator::up(&db, None).await;
        if let Err(e) = resolve_auto_migration(outcome, best_effort) {
            eprintln!("suprnova: migration failed: {e}");
            eprintln!(
                "suprnova: refusing to start the server against a partially-migrated schema. \
                 Fix the failing migration, or set {AUTO_MIGRATE_BEST_EFFORT_ENV}=true to keep \
                 the previous best-effort behaviour, or pass --no-migrate to skip auto-migration."
            );
            std::process::exit(1);
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

        // Long-lived JoinSet for `.run_in_background()` tasks. These tasks
        // are fire-and-forget within a tick — the loop polls completed ones
        // before each tick and on shutdown awaits the rest before exit, so a
        // slow background task never gets dropped mid-flight.
        let mut bg_tasks: tokio::task::JoinSet<crate::schedule::ScheduledTaskJoin> =
            tokio::task::JoinSet::new();

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    // Surface any background tasks that completed since the
                    // last tick. `try_join_next` is non-blocking — anything
                    // still running stays in the set for the next sweep.
                    while let Some(joined) = bg_tasks.try_join_next() {
                        report_background_outcome(joined);
                    }
                    // Run this tick's due tasks. Inline tasks complete
                    // before we return; `run_in_background` tasks land in
                    // `bg_tasks` and are observed on the next tick or at
                    // shutdown.
                    for (name, result) in schedule.run_due_tasks_into(&mut bg_tasks).await {
                        if let Err(e) = result {
                            eprintln!("suprnova: scheduled task '{name}' failed: {e}");
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    println!("suprnova: scheduler shutting down.");
                    if !bg_tasks.is_empty() {
                        println!(
                            "suprnova: waiting for {} background task(s) to finish…",
                            bg_tasks.len()
                        );
                    }
                    while let Some(joined) = bg_tasks.join_next().await {
                        report_background_outcome(joined);
                    }
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

        let worker = crate::workflow::WorkflowWorker::new();
        let cancel = tokio_util::sync::CancellationToken::new();

        println!("==============================================");
        println!("  suprnova Workflow Worker");
        println!("==============================================");
        println!("  worker_id: {}", worker.worker_id());
        println!("  Press Ctrl+C to stop (in-flight workflows will drain)");
        println!("==============================================");

        // Mirror the queue worker shutdown pattern: spawn the worker on a
        // task so we can race it against Ctrl-C without blocking the
        // signal future. On signal we cancel the token and await the
        // task; the worker's drain loop awaits every in-flight workflow
        // before returning Ok(()).
        let cancel_for_worker = cancel.clone();
        let mut handle =
            tokio::spawn(async move { worker.run_with_cancel(cancel_for_worker).await });

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("suprnova: workflow worker shutting down (Ctrl-C).");
                cancel.cancel();
                match handle.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        eprintln!("Workflow worker error during drain: {e}");
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("Workflow worker task panicked during drain: {e}");
                        std::process::exit(1);
                    }
                }
            }
            res = &mut handle => {
                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        eprintln!("Workflow worker error: {e}");
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("Workflow worker task panicked: {e}");
                        std::process::exit(1);
                    }
                }
            }
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

    /// `schedule:run` semantics: a `.run_in_background()` task must still
    /// surface in the returned `(results, any_failed)` tuple, so the
    /// handler reports success/failure consistently regardless of how the
    /// task was executed.
    #[tokio::test]
    async fn evaluate_due_once_drains_background_tasks_before_returning() {
        let f: ScheduleFn = Box::new(|sched: &mut Schedule| {
            let b = sched
                .call(|| async { Ok(()) })
                .every_minute()
                .name("inline-ok");
            sched.add(b);
            let b = sched
                .call(|| async { Ok(()) })
                .every_minute()
                .name("bg-ok")
                .run_in_background();
            sched.add(b);
            let b = sched
                .call(|| async { Err(FrameworkError::internal("bg-failure")) })
                .every_minute()
                .name("bg-err")
                .run_in_background();
            sched.add(b);
        });
        let schedule = build_schedule(Some(f));
        let (results, any_failed) = evaluate_due_once(&schedule).await;
        assert_eq!(results.len(), 3);
        assert!(any_failed, "a failing background task must flip any_failed");

        let by_name: std::collections::BTreeMap<_, _> = results
            .iter()
            .map(|(n, r)| (n.as_str(), r.is_ok()))
            .collect();
        assert_eq!(by_name.get("inline-ok"), Some(&true));
        assert_eq!(by_name.get("bg-ok"), Some(&true));
        assert_eq!(by_name.get("bg-err"), Some(&false));
    }

    /// `Application::new().run()` defaults to `NoMigrator`, whose
    /// `migrations()` returns an empty vec. The default `serve`/`web:run`
    /// arm calls `run_migrations_silent::<M>()` before booting the server;
    /// a framework app without a database must boot without `DATABASE_URL`
    /// being set.
    ///
    /// Without the empty-migrations short-circuit in `run_migrations_silent`,
    /// `get_database_connection()` calls `std::process::exit(1)` when the
    /// env var is missing — that would terminate the entire test binary,
    /// not just fail this single test, so a passing run is itself the
    /// regression signal.
    ///
    /// The `remove_var` is load-bearing: if the ambient environment has
    /// `DATABASE_URL` set, the unfixed path would skip the exit and
    /// silently succeed, making the test green against the bug. We gate
    /// with `#[serial_test::serial]` because the env is process-wide.
    #[tokio::test]
    #[serial_test::serial]
    async fn no_migrator_default_serve_does_not_require_database_url() {
        let prior = env::var("DATABASE_URL").ok();
        // SAFETY: edition 2024 marks env mutation `unsafe`; we serialize
        // with `#[serial_test::serial]` so no concurrent test reads it,
        // and we restore the prior value before returning.
        unsafe {
            env::remove_var("DATABASE_URL");
        }

        // With the fix in place this call returns immediately because
        // `NoMigrator::migrations()` is empty; without the fix this would
        // terminate the test binary via `std::process::exit(1)` inside
        // `get_database_connection`.
        Application::<NoMigrator>::run_migrations_silent::<NoMigrator>().await;

        // SAFETY: same justification as above.
        unsafe {
            if let Some(prior) = prior {
                env::set_var("DATABASE_URL", prior);
            }
        }
    }

    /// `SUPRNOVA_AUTO_MIGRATE_BEST_EFFORT` parsing: unset, empty, and
    /// non-truthy values must keep the production-safe fail-closed default.
    #[test]
    fn parse_auto_migrate_best_effort_defaults_to_false() {
        assert!(!parse_auto_migrate_best_effort(None));
        assert!(!parse_auto_migrate_best_effort(Some("")));
        assert!(!parse_auto_migrate_best_effort(Some("   ")));
        assert!(!parse_auto_migrate_best_effort(Some("false")));
        assert!(!parse_auto_migrate_best_effort(Some("0")));
        assert!(!parse_auto_migrate_best_effort(Some("no")));
        assert!(!parse_auto_migrate_best_effort(Some("off")));
    }

    /// The full truthy alphabet: `true` / `1` / `yes` / `on`, mixed case,
    /// trimmed of surrounding whitespace.
    #[test]
    fn parse_auto_migrate_best_effort_accepts_truthy_values() {
        for v in [
            "true", "TRUE", "True", "  true  ", "1", " 1 ", "yes", "YES", "on", "On",
        ] {
            assert!(
                parse_auto_migrate_best_effort(Some(v)),
                "{v:?} should enable best-effort mode",
            );
        }
    }

    /// Success outcomes pass through both modes; this pins the contract so
    /// future refactors can't accidentally swap the arms.
    #[test]
    fn resolve_auto_migration_passes_success_through() {
        assert!(resolve_auto_migration(Ok(()), false).is_ok());
        assert!(resolve_auto_migration(Ok(()), true).is_ok());
    }

    /// Regression for `app-serve-fails-open`: with the default
    /// (best_effort=false), a migration error must propagate so the caller
    /// (`run_migrations_silent`) can `exit(1)` instead of booting the server
    /// against a half-migrated schema.
    #[test]
    fn resolve_auto_migration_default_fails_closed_on_error() {
        let err = sea_orm::DbErr::Migration("create_users_table: column already exists".into());
        let outcome = resolve_auto_migration(Err(err), false);
        assert!(
            outcome.is_err(),
            "default mode must surface the migration error so the server aborts",
        );
    }

    /// Best-effort opt-in (the SUPRNOVA_AUTO_MIGRATE_BEST_EFFORT=true escape
    /// hatch) preserves the legacy log-and-continue behaviour for operators
    /// who explicitly want it.
    #[test]
    fn resolve_auto_migration_best_effort_swallows_error() {
        let err = sea_orm::DbErr::Migration("create_users_table: column already exists".into());
        let outcome = resolve_auto_migration(Err(err), true);
        assert!(
            outcome.is_ok(),
            "best-effort mode must swallow the migration error so the server still boots",
        );
    }

    /// End-to-end variant of the fix: route a real `Migrator::up` failure
    /// through the same policy gate `run_migrations_silent` uses. Uses a
    /// migrator whose first migration deliberately fails so the result is a
    /// real `DbErr`, not a hand-rolled one. Connects to `sqlite::memory:`
    /// directly to avoid `get_database_connection`'s `exit(1)` on missing
    /// `DATABASE_URL`.
    #[tokio::test]
    async fn resolve_auto_migration_routes_real_migrator_failure() {
        struct FailingMigration;

        impl MigrationName for FailingMigration {
            fn name(&self) -> &str {
                "m_app_serve_fails_open_regression_failing_migration"
            }
        }

        #[async_trait]
        impl MigrationTrait for FailingMigration {
            async fn up(&self, _manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
                Err(sea_orm::DbErr::Migration(
                    "intentional failure for app-serve-fails-open regression test".into(),
                ))
            }

            async fn down(&self, _manager: &SchemaManager) -> Result<(), sea_orm::DbErr> {
                Ok(())
            }
        }

        struct FailingMigrator;

        #[async_trait]
        impl MigratorTrait for FailingMigrator {
            fn migrations() -> Vec<Box<dyn MigrationTrait>> {
                vec![Box::new(FailingMigration)]
            }
        }

        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("in-memory sqlite should connect");

        // Default mode: server boot would abort.
        let outcome = FailingMigrator::up(&db, None).await;
        assert!(
            resolve_auto_migration(outcome, false).is_err(),
            "default fail-closed mode must propagate a real Migrator::up failure",
        );

        // Best-effort opt-in: same error, swallowed.
        let outcome = FailingMigrator::up(&db, None).await;
        assert!(
            resolve_auto_migration(outcome, true).is_ok(),
            "best-effort opt-in must swallow a real Migrator::up failure",
        );
    }
}
