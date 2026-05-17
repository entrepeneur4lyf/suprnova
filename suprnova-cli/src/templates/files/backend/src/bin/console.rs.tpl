//! {package_name} console — runtime command dispatch.
//!
//! Per-project entry point for `db:seed`, your own `#[command]`s, and
//! other one-shot CLI tasks. Calls `{package_name}::bootstrap::register()`
//! so seeders, queue jobs, mail factories, etc. are wired up before
//! dispatch, then routes argv to a registered console command.
//!
//! ```text
//! cargo run --bin console -- db:seed
//! cargo run --bin console -- help
//! ./target/debug/console <your-command>
//! ```
//!
//! Tokio flavor is `current_thread` — console commands are one-shot,
//! so the multi-threaded worker pool would buy nothing.

use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // Load `.env` so configs read DATABASE_URL etc.
    let _ = dotenvy::dotenv();

    // Mirror the server's `Application::new().config(...).bootstrap(...)`
    // ordering — DB init in bootstrap reads what config registered.
    {package_name}::config::register_all();
    {package_name}::bootstrap::register().await;

    let argv: Vec<String> = std::env::args().collect();
    match suprnova::console::dispatch_argv(argv).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {{e}}");
            ExitCode::FAILURE
        }
    }
}
