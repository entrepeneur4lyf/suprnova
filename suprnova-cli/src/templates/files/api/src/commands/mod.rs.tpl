//! Application-defined console commands.
//!
//! Each module registers an `#[command]`-annotated async fn via
//! inventory; the console binary in `src/bin/console.rs` dispatches
//! to them automatically. Use `suprnova make:command <name>` to
//! generate a new command file with the right shape.
