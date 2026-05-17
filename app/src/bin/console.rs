//! Suprnova app console — runtime command dispatch for this project.
//!
//! Per-project entry point for runtime CLI commands: `db:seed`,
//! user-defined commands registered via `#[command]`, etc. Calls
//! `app::bootstrap::register()` so the seeder / queue / event
//! registries are wired up before dispatch, then routes argv to a
//! handler registered in `suprnova::console`'s inventory.
//!
//! Why a separate binary from `app` (the HTTP server in `cmd/main.rs`):
//! `app` starts the listener and never returns. A console command
//! should exit when its handler returns. Same crate, same bootstrap,
//! different `fn main`.
//!
//! Usage:
//!
//! ```text
//! cargo run --bin console -- db:seed
//! cargo run --bin console -- greet alice
//! ./target/debug/console help
//! ```
//!
//! Tokio runtime flavor is `current_thread` — console commands are
//! one-shot, so the multi-threaded worker pool overhead would buy
//! nothing.

use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // Load `.env` first so config::register_all sees DATABASE_URL etc.
    // The server's `Application::run` does this for us; the console
    // binary takes the same responsibility on directly. Ignore the
    // "no .env" result — env may already be populated by the shell.
    let _ = dotenvy::dotenv();

    // Register configs, then run the same bootstrap the HTTP server
    // runs. Matches the `Application::new().config(...).bootstrap(...)`
    // ordering in `cmd/main.rs` — DB init in bootstrap reads the
    // DatabaseConfig that `register_all` parks in `Config::register`.
    app::config::register_all();
    app::bootstrap::register().await;

    let argv: Vec<String> = std::env::args().collect();
    // dispatch_argv owns all user-facing stderr output (both clap
    // parse errors and handler-returned errors). Main is pure
    // Result → ExitCode translation.
    match suprnova::console::dispatch_argv(argv).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
