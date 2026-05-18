//! {project_name} console — runtime command dispatch.
//!
//! Per-project entry point for `db:seed`, your own `#[command]`s, and
//! other one-shot CLI tasks. Calls `{package_name}::bootstrap::register()`
//! lazily (only when a real subcommand matches), then routes argv to
//! a registered console command.
//!
//! ```text
//! cargo run --bin console -- db:seed
//! cargo run --bin console -- --version
//! cargo run --bin console -- help
//! ./target/debug/console <your-command>
//! ```

use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let _ = dotenvy::dotenv();

    // Surface this project's package version via `--version` and
    // `--help`.
    suprnova::console::set_version(env!("CARGO_PKG_VERSION"));

    let argv: Vec<String> = std::env::args().collect();
    // dispatch_argv_with_init owns all user-facing stderr (both clap
    // parse errors and handler-returned errors); main is pure
    // Result → ExitCode translation. The bootstrap closure runs only
    // when clap matches a real registered subcommand — help, version,
    // and parse-error paths skip it entirely.
    let result = suprnova::console::dispatch_argv_with_init(argv, || async {
        {package_name}::config::register_all();
        {package_name}::bootstrap::register().await;
    })
    .await;

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
