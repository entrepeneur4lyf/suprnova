//! Application-defined console commands.
//!
//! Each module registers a typed `#[derive(Command)]` struct (or a
//! raw `#[command]` fn) via inventory; the console binary in
//! `src/bin/console.rs` dispatches to them automatically. Use
//! `suprnova make:command <name>` to scaffold a new command with
//! the right shape.
