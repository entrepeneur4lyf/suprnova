//! Application-defined console commands.
//!
//! Each module registers an `#[command]`-annotated async fn via
//! inventory, so the console binary (`src/bin/console.rs`) dispatches
//! to them without needing per-command wiring.
//!
//! Add a new command:
//!
//! ```text
//! suprnova make:command clean-cache
//! ```
//!
//! That generates `src/commands/clean_cache.rs` and appends a `pub mod`
//! line here. Or do it by hand — drop a file, add the line.
