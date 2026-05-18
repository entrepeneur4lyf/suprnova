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
    let _ = dotenvy::dotenv();

    let argv: Vec<String> = std::env::args().collect();
    // Bootstrap runs only when a real subcommand is matched — help,
    // version, missing-subcommand, and parse-error paths all skip
    // it. That's how `console --help` works without DATABASE_URL
    // set (DB::init would panic during bootstrap otherwise).
    //
    // dispatch_argv_with_init owns all user-facing stderr output
    // (both clap parse errors and handler-returned errors); main is
    // pure Result → ExitCode translation.
    let result = suprnova::console::dispatch_argv_with_init(argv, || async {
        app::config::register_all();
        app::bootstrap::register().await;
    })
    .await;

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
