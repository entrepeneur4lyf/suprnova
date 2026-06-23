//! Typed console commands.
//!
//! [`TypedCommand`] is the trait struct-shaped commands implement to
//! receive parsed, typed arguments instead of a raw `Vec<String>`.
//! Combined with `#[derive(Command)]`, the user writes:
//!
//! ```rust,no_run
//! use async_trait::async_trait;
//! use suprnova::{Command, FrameworkError, TypedCommand};
//!
//! #[derive(clap::Parser, Command)]
//! #[console(name = "greet", description = "Greet someone")]
//! pub struct Greet {
//!     #[arg(short, long)]
//!     name: Option<String>,
//!     #[arg(long)]
//!     loud: bool,
//! }
//!
//! #[async_trait]
//! impl TypedCommand for Greet {
//!     async fn run(self) -> Result<(), FrameworkError> {
//!         let target = self.name.unwrap_or_else(|| "world".into());
//!         let prefix = if self.loud { "HELLO" } else { "Hello" };
//!         println!("{prefix}, {target}!");
//!         Ok(())
//!     }
//! }
//! ```
//!
//! The derive generates everything between the struct definition and
//! the user's `impl TypedCommand`: the clap subcommand builder, the
//! ArgMatches adapter, and the `inventory::submit!` registration.

use crate::error::FrameworkError;
use async_trait::async_trait;

/// A console command driven by typed args parsed via clap.
///
/// Implementors must also derive `clap::Parser` (so the framework can
/// build a subcommand schema from the struct fields) and
/// `suprnova::Command` (so the framework wires the registration).
///
/// The `run` method consumes `self` so the parsed args are owned and
/// can be moved into spawned tasks freely; the framework owns no
/// long-lived reference to the parsed struct after `run` returns.
#[async_trait]
pub trait TypedCommand: Sized + Send {
    /// Execute the subcommand. Consumes `self` so the parsed args can
    /// move into spawned tasks freely.
    async fn run(self) -> Result<(), FrameworkError>;
}
