//! `greet` console command — integration test via `dispatch_argv`.
//!
//! The dogfood greet command uses the typed-command path
//! (`#[derive(Command)] + TypedCommand`). This test pins:
//!
//!   - linking the app crate's `commands` module triggers the
//!     derive's inventory submission so `console::find("greet")`
//!     resolves
//!   - `dispatch_argv(["console", "greet", ...])` routes through
//!     clap → `FromArgMatches` → `TypedCommand::run`
//!   - the `Greet` struct itself stays directly callable via its
//!     `TypedCommand::run` impl (so app-level unit tests of console
//!     handlers don't need to thread argv strings)
//!
//! Spawning the actual `console` binary as a subprocess is doable
//! but overkill — the framework's dispatch path is the contract;
//! the binary just hands argv to it. The framework's own tests
//! cover the binary-equivalent path.

use app::commands::greet::Greet;
use suprnova::{console, TypedCommand};

#[tokio::test]
async fn greet_is_registered_via_derive() {
    let entry = console::find("greet")
        .expect("greet must be registered when app::commands is linked");
    assert_eq!(entry.name, "greet");
    assert_eq!(entry.description, "Print a friendly greeting");
}

#[tokio::test]
async fn greet_runs_via_dispatch_argv_with_typed_flags() {
    let argv = vec![
        "console".to_string(),
        "greet".to_string(),
        "--name".to_string(),
        "Suprnova".to_string(),
    ];
    console::dispatch_argv(argv)
        .await
        .expect("greet succeeds with --name");
}

#[tokio::test]
async fn greet_struct_callable_directly_via_typed_command() {
    // The Greet struct is reachable as a regular Rust type, so
    // unit-style tests can build it directly without going through
    // clap or dispatch_argv.
    let cmd = Greet {
        name: "Alice".to_string(),
        loud: false,
    };
    cmd.run().await.expect("direct .run() returns Ok");

    let loud = Greet {
        name: "Bob".to_string(),
        loud: true,
    };
    loud.run().await.expect("loud .run() returns Ok");
}
