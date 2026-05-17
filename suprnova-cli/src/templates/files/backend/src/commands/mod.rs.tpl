//! Application-defined console commands.
//!
//! Each module registers a typed `#[derive(Command)]` struct (or a
//! raw `#[command]` fn) via inventory, so the console binary
//! (`src/bin/console.rs`) dispatches to them without needing
//! per-command wiring.
//!
//! Add a new command:
//!
//! ```text
//! suprnova make:command clean-cache
//! ```
//!
//! That generates `src/commands/clean_cache.rs` with a
//! `#[derive(clap::Parser, suprnova::Command)]` stub + a
//! `TypedCommand` impl, and appends a `pub mod` line here.
