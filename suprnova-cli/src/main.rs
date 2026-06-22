mod commands;
mod templates;
pub mod ui;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "suprnova")]
#[command(about = "A CLI for scaffolding Suprnova web applications", long_about = None)]
#[command(disable_help_flag = true)]
#[command(disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Print help
    #[arg(short, long, global = true)]
    help: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new Suprnova project
    New {
        /// The name of the project to create
        name: Option<String>,

        /// Skip all prompts and use defaults
        #[arg(long)]
        no_interaction: bool,

        /// Skip git initialization
        #[arg(long)]
        no_git: bool,

        /// Frontend framework (svelte, react, vue). Prompts if omitted.
        /// Conflicts with --api.
        #[arg(long, conflicts_with = "api")]
        frontend: Option<String>,

        /// Scaffold a JSON:API-only project (no Inertia, no frontend).
        #[arg(long)]
        api: bool,

        /// Emit a portless.json so `suprnova dev:tls` can serve the app
        /// at https://<name>.localhost. Opt-in; nothing else changes.
        #[arg(long)]
        with_portless: bool,
    },
    /// Start the development servers (backend + frontend)
    Serve {
        /// Backend port. Overrides SERVER_PORT/.env and pins the port
        /// exactly (no free-port scan). Defaults to SERVER_PORT, else
        /// 8765, scanning upward if that port is busy.
        #[arg(long, short = 'p')]
        port: Option<u16>,

        /// Frontend (Vite) port. Overrides VITE_PORT/.env and pins the
        /// port exactly. Defaults to VITE_PORT, else 5765, scanning
        /// upward if that port is busy.
        #[arg(long)]
        frontend_port: Option<u16>,

        /// Only start backend server
        #[arg(long, conflicts_with = "frontend_only")]
        backend_only: bool,

        /// Only start frontend server
        #[arg(long)]
        frontend_only: bool,

        /// Skip TypeScript type generation
        #[arg(long)]
        skip_types: bool,
    },
    /// Register an HTTPS dev URL (https://<name>.localhost) and trust
    /// portless's CA in your browsers' certificate stores
    #[command(name = "dev:tls")]
    DevTls {
        /// App name for the URL. Defaults to the project's Cargo.toml
        /// package name.
        #[arg(long)]
        name: Option<String>,

        /// Backend port to route to. Defaults to SERVER_PORT, else 8765.
        #[arg(long, short = 'p')]
        port: Option<u16>,

        /// Only trust the CA; skip registering the portless route.
        #[arg(long)]
        no_alias: bool,
    },
    /// Run the web server (app runtime)
    #[command(name = "web:run")]
    WebRun,
    /// Generate TypeScript types from Rust InertiaProps structs
    GenerateTypes {
        /// Output file path (default: frontend/src/types/inertia-props.ts)
        #[arg(long, short = 'o')]
        output: Option<String>,

        /// Watch for changes and regenerate
        #[arg(long, short = 'w')]
        watch: bool,
    },
    /// Generate a new middleware
    #[command(name = "make:middleware")]
    MakeMiddleware {
        /// Name of the middleware (e.g., Auth, RateLimit)
        name: String,
    },
    /// Generate a new controller
    #[command(name = "make:controller")]
    MakeController {
        /// Name of the controller (e.g., users, user_profile)
        name: String,
    },
    /// Generate a new action
    #[command(name = "make:action")]
    MakeAction {
        /// Name of the action (e.g., AddTodo, CreateUser)
        name: String,
    },
    /// Generate a new console command
    #[command(name = "make:command")]
    MakeCommand {
        /// Name of the command (e.g., `clean-cache`, `mail:send`, `CleanCache`)
        name: String,
    },
    /// Generate a new domain error
    #[command(name = "make:error")]
    MakeError {
        /// Name of the error (e.g., UserNotFound, InvalidInput)
        name: String,
    },
    /// Generate a new Inertia page or Data struct
    #[command(name = "make:inertia")]
    MakeInertia {
        /// Name of the page or struct (e.g., About, UserProps)
        name: String,
        /// Scaffold a #[derive(Data, Validate)] struct in app/src/props/ instead of a frontend page
        #[arg(long)]
        data: bool,
    },
    /// Generate a new database migration
    #[command(name = "make:migration")]
    MakeMigration {
        /// Name of the migration (e.g., create_users_table, add_email_to_users)
        name: String,
    },
    /// Generate a new scheduled task
    #[command(name = "make:task")]
    MakeTask {
        /// Name of the task (e.g., CleanupLogs, SendReminders)
        name: String,
    },
    /// Run all pending database migrations
    Migrate,
    /// Rollback the last database migration(s)
    #[command(name = "migrate:rollback")]
    MigrateRollback {
        /// Number of migrations to rollback
        #[arg(long, default_value = "1")]
        step: u32,
    },
    /// Show the status of all migrations
    #[command(name = "migrate:status")]
    MigrateStatus,
    /// Drop all tables and re-run all migrations
    #[command(name = "migrate:fresh")]
    MigrateFresh,
    /// Sync database schema to entity files (runs migrations + generates entities)
    #[command(name = "db:sync")]
    DbSync {
        /// Skip running migrations before sync
        #[arg(long)]
        skip_migrations: bool,
        /// Regenerate model files (overwrites existing custom models with new Eloquent-like API)
        #[arg(long)]
        regenerate_models: bool,
    },
    /// Generate a production-ready Dockerfile
    #[command(name = "docker:init")]
    DockerInit,
    /// Generate docker-compose.yml for local development
    #[command(name = "docker:compose")]
    DockerCompose {
        /// Include Mailpit email testing service
        #[arg(long)]
        with_mailpit: bool,
        /// Include MinIO S3-compatible storage service
        #[arg(long)]
        with_minio: bool,
    },
    /// Run all due scheduled tasks once (typically called by cron every minute)
    #[command(name = "schedule:run")]
    ScheduleRun,
    /// Start the scheduler daemon (runs continuously, checks every minute)
    #[command(name = "schedule:work")]
    ScheduleWork,
    /// List all registered scheduled tasks
    #[command(name = "schedule:list")]
    ScheduleList,
    /// Start the workflow worker daemon
    #[command(name = "workflow:work")]
    WorkflowWork,
    /// Install workflow migrations
    #[command(name = "workflow:install")]
    WorkflowInstall,
    /// Launch the Inertia SSR worker in the foreground
    #[command(name = "ssr:start")]
    SsrStart {
        /// Runtime to launch the worker under (node, bun, deno).
        /// Falls back to SUPRNOVA_SSR_RUNTIME env, then "node".
        #[arg(long)]
        runtime: Option<String>,
        /// Path to the built SSR bundle. Falls back to
        /// SUPRNOVA_SSR_BUNDLE env, then frontend/bootstrap/ssr/ssr.js.
        #[arg(long)]
        bundle: Option<String>,
    },
    /// Verify the Inertia SSR worker is reachable
    #[command(name = "ssr:check")]
    SsrCheck {
        /// SSR worker URL. Falls back to SUPRNOVA_SSR_URL env, then
        /// http://127.0.0.1:13714.
        #[arg(long)]
        url: Option<String>,
        /// Connect timeout in milliseconds.
        #[arg(long, default_value = "2000")]
        timeout_ms: u64,
    },
    /// Generate a new APP_KEY (32-byte AES-256, base64 URL-safe, no padding)
    #[command(name = "key:generate")]
    KeyGenerate {
        /// Print only the key (no surrounding hint text). Suitable for
        /// `APP_KEY=$(suprnova key:generate --show)`.
        #[arg(long)]
        show: bool,
    },
}

fn main() {
    let cli = Cli::parse();

    if cli.help && cli.command.is_none() {
        ui::print_help();
        return;
    }

    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            ui::print_help();
            return;
        }
    };

    match command {
        Commands::New {
            name,
            no_interaction,
            no_git,
            frontend,
            api,
            with_portless,
        } => {
            commands::new::run(name, no_interaction, no_git, frontend, api, with_portless);
        }
        Commands::Serve {
            port,
            frontend_port,
            backend_only,
            frontend_only,
            skip_types,
        } => {
            commands::serve::run(port, frontend_port, backend_only, frontend_only, skip_types);
        }
        Commands::DevTls {
            name,
            port,
            no_alias,
        } => {
            commands::dev_tls::run(name, port, no_alias);
        }
        Commands::WebRun => {
            commands::web_run::run();
        }
        Commands::GenerateTypes { output, watch } => {
            commands::generate_types::run(output, watch);
        }
        Commands::MakeMiddleware { name } => {
            commands::make_middleware::run(name);
        }
        Commands::MakeController { name } => {
            commands::make_controller::run(name);
        }
        Commands::MakeAction { name } => {
            commands::make_action::run(name);
        }
        Commands::MakeCommand { name } => {
            commands::make_command::run(name);
        }
        Commands::MakeError { name } => {
            commands::make_error::run(name);
        }
        Commands::MakeInertia { name, data } => {
            commands::make_inertia::run(name, data);
        }
        Commands::MakeMigration { name } => {
            commands::make_migration::run(name);
        }
        Commands::MakeTask { name } => {
            commands::make_task::run(name);
        }
        Commands::Migrate => {
            commands::migrate::run();
        }
        Commands::MigrateRollback { step } => {
            commands::migrate_rollback::run(step);
        }
        Commands::MigrateStatus => {
            commands::migrate_status::run();
        }
        Commands::MigrateFresh => {
            commands::migrate_fresh::run();
        }
        Commands::DbSync {
            skip_migrations,
            regenerate_models,
        } => {
            commands::db_sync::run(skip_migrations, regenerate_models);
        }
        Commands::DockerInit => {
            commands::docker_init::run();
        }
        Commands::DockerCompose {
            with_mailpit,
            with_minio,
        } => {
            commands::docker_compose::run(with_mailpit, with_minio);
        }
        Commands::ScheduleRun => {
            commands::schedule_run::run();
        }
        Commands::ScheduleWork => {
            commands::schedule_work::run();
        }
        Commands::ScheduleList => {
            commands::schedule_list::run();
        }
        Commands::WorkflowWork => {
            commands::workflow_work::run();
        }
        Commands::WorkflowInstall => {
            commands::workflow_install::run();
        }
        Commands::SsrStart { runtime, bundle } => {
            commands::ssr_start::run(runtime, bundle);
        }
        Commands::SsrCheck { url, timeout_ms } => {
            commands::ssr_check::run(url, timeout_ms);
        }
        Commands::KeyGenerate { show } => {
            commands::key_generate::run(show);
        }
    }
}
