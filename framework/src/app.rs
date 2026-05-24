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

use crate::{Config, Router, Server};
use clap::{Parser, Subcommand};
use sea_orm_migration::prelude::*;
use std::env;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

/// Boxed async bootstrap function (avoids repeating the complex trait-object type).
type BootstrapFn = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

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
            _migrator: std::marker::PhantomData,
        }
    }
}

impl Default for Application<NoMigrator> {
    fn default() -> Self {
        Self::new()
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
                Self::run_server_internal(bootstrap_fn, routes_fn).await;
            }
            Some(Commands::Serve { no_migrate: true })
            | Some(Commands::WebRun { no_migrate: true }) => {
                // Run server without migrations
                Self::run_server_internal(bootstrap_fn, routes_fn).await;
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
                Self::run_scheduler_daemon_internal(bootstrap_fn).await;
            }
            Some(Commands::ScheduleRun) => {
                Self::run_scheduled_tasks_internal(bootstrap_fn).await;
            }
            Some(Commands::ScheduleList) => {
                Self::list_scheduled_tasks().await;
            }
            Some(Commands::WorkflowWork) => {
                Self::run_workflow_worker_internal(bootstrap_fn).await;
            }
        }
    }

    async fn run_server_internal(
        bootstrap_fn: Option<BootstrapFn>,
        routes_fn: Option<Box<dyn FnOnce() -> Router + Send>>,
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
        // This is the boot-time fail-closed path described in codex
        // review finding #1.
        let server = match Server::from_config(router) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("suprnova: failed to start server: {e}");
                std::process::exit(1);
            }
        };
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
                && !parent.as_os_str().is_empty() {
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
            .expect("Failed to connect to database")
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
        Migrator::up(&db, None)
            .await
            .expect("Failed to run migrations");
        println!("Migrations completed successfully!");
    }

    async fn show_migration_status<Migrator: MigratorTrait>() {
        println!("Migration status:");
        let db = Self::get_database_connection().await;
        Migrator::status(&db)
            .await
            .expect("Failed to get migration status");
    }

    async fn rollback_migrations<Migrator: MigratorTrait>(steps: u32) {
        println!("Rolling back {} migration(s)...", steps);
        let db = Self::get_database_connection().await;
        Migrator::down(&db, Some(steps))
            .await
            .expect("Failed to rollback migrations");
        println!("Rollback completed successfully!");
    }

    async fn fresh_migrations<Migrator: MigratorTrait>() {
        println!("WARNING: Dropping all tables and re-running migrations...");
        let db = Self::get_database_connection().await;
        Migrator::fresh(&db)
            .await
            .expect("Failed to refresh database");
        println!("Database refreshed successfully!");
    }

    async fn run_scheduler_daemon_internal(
        bootstrap_fn: Option<BootstrapFn>,
    ) {
        // Run bootstrap for scheduler context
        if let Some(bootstrap_fn) = bootstrap_fn {
            bootstrap_fn().await;
        }

        println!("==============================================");
        println!("  suprnova Scheduler Daemon");
        println!("==============================================");
        println!();
        println!("  Note: Create tasks with `suprnova make:task <name>`");
        println!("  Press Ctrl+C to stop");
        println!();
        println!("==============================================");

        eprintln!("Scheduler daemon is not yet configured.");
        eprintln!("Create a scheduled task with: suprnova make:task <name>");
        eprintln!("Then register it in src/schedule.rs");
    }

    async fn run_scheduled_tasks_internal(
        bootstrap_fn: Option<BootstrapFn>,
    ) {
        // Run bootstrap for scheduler context
        if let Some(bootstrap_fn) = bootstrap_fn {
            bootstrap_fn().await;
        }

        println!("Running scheduled tasks...");
        eprintln!("Scheduler is not yet configured.");
        eprintln!("Create a scheduled task with: suprnova make:task <name>");
    }

    async fn list_scheduled_tasks() {
        println!("Registered scheduled tasks:");
        println!();
        eprintln!("No scheduled tasks registered.");
        eprintln!("Create a scheduled task with: suprnova make:task <name>");
    }

    async fn run_workflow_worker_internal(
        bootstrap_fn: Option<BootstrapFn>,
    ) {
        // Audit fix: workflow workers must see the same Cache / Queue /
        // RateLimit / Mail drivers as the web server. Without this the
        // worker would silently default to in-memory queue + in-memory
        // cache even when QUEUE_DRIVER=redis was set in the environment,
        // because `Server::run`'s driver bootstrap is never reached when
        // booting through `workflow:work`. Drivers are bootstrapped
        // BEFORE the user's bootstrap_fn so user code can register
        // queue handlers, attach to cache, etc., against live drivers.
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

    /// Shared bootstrap for non-server subcommands that still need the
    /// runtime drivers: Cache, Queue, RateLimit, Mail. Mirrors the
    /// driver-bootstrap order in `Server::run` (telemetry / encryption
    /// keys / authorization init are subcommand-specific and stay out
    /// of this helper).
    async fn bootstrap_runtime_drivers(
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        crate::cache::Cache::bootstrap().await?;
        crate::queue::bootstrap_from_env().await?;
        crate::rate_limit::bootstrap_from_env().await?;
        crate::mail::boot::bootstrap_from_env()?;
        Ok(())
    }
}
