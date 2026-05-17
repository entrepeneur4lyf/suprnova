//! `greet` — dogfood for the typed-command path (`#[derive(Command)]`
//! + `TypedCommand`).
//!
//! Demonstrates the structured shape every non-trivial console
//! command should use: clap's `Parser` derive describes the args,
//! our `Command` derive registers the command + adapter, the trait
//! impl is the body.
//!
//! ```text
//! cargo run --bin console -- greet                   # "Hello, world!"
//! cargo run --bin console -- greet --name Alice      # "Hello, Alice!"
//! cargo run --bin console -- greet -n Alice --loud   # "HELLO, Alice!"
//! cargo run --bin console -- greet --help            # per-command help
//! ```

use async_trait::async_trait;
use clap::Parser;
use suprnova::{Command, FrameworkError, TypedCommand};

#[derive(Parser, Command, Debug)]
#[console(name = "greet", description = "Print a friendly greeting")]
pub struct Greet {
    /// Who to greet. Defaults to "world".
    #[arg(short, long, default_value = "world")]
    pub name: String,

    /// Use the uppercase greeting.
    #[arg(long, default_value_t = false)]
    pub loud: bool,
}

#[async_trait]
impl TypedCommand for Greet {
    async fn run(self) -> Result<(), FrameworkError> {
        let prefix = if self.loud { "HELLO" } else { "Hello" };
        println!("{prefix}, {name}!", name = self.name);
        Ok(())
    }
}
