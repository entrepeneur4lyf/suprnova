//! {project_name} console — runtime command dispatch.
//!
//! Per-project entry point for `db:seed`, your own `#[command]`s, and
//! other one-shot CLI tasks. Calls `{package_name}::bootstrap::register()`
//! so seeders, queue jobs, etc. are wired before dispatch, then routes
//! argv to a registered console command.
//!
//! ```text
//! cargo run --bin console -- db:seed
//! cargo run --bin console -- help
//! ./target/debug/console <your-command>
//! ```

use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let _ = dotenvy::dotenv();

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
