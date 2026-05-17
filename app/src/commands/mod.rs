//! Application-defined console commands. Each module registers an
//! `#[command]`-annotated async fn into the framework's inventory, so
//! the console binary (`src/bin/console.rs`) dispatches to them
//! without needing per-command wiring.
//!
//! Adding a new command: drop a file in this directory, add a `pub mod`
//! line here, write `#[command(name = "...")] async fn ...`. The
//! `suprnova make:command` generator does this for you.

pub mod greet;
