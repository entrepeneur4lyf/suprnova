//! `suprnova console model:prune` — scheduled cleanup of stale rows.
//!
//! Walks the [`PrunerEntry`][crate::eloquent::PrunerEntry] registry
//! and force-deletes every row each registered Prunable /
//! MassPrunable's scope returns. With `--model=Name` the runner
//! restricts to a single type; with `--pretend` it reports the
//! rowcount that would be deleted without modifying any rows.
//!
//! Typed command shape — uses `#[derive(clap::Parser)] +
//! #[derive(Command)] + impl TypedCommand` per Phase 6B's typed-args
//! console pattern. The struct is the source of truth for clap's
//! argument parsing; the `impl TypedCommand` body owns the runtime
//! dispatch.

use async_trait::async_trait;
use suprnova_macros::Command;

use crate::console::TypedCommand;
use crate::error::FrameworkError;

#[derive(clap::Parser, Debug, Command)]
#[console(
    name = "model:prune",
    description = "Prune stale rows for every Prunable / MassPrunable model."
)]
pub struct PruneArgs {
    /// Restrict to a single model type. Matches against the type
    /// name's last path segment (e.g. `User`, not `crate::models::User`).
    #[arg(long)]
    pub model: Option<String>,

    /// Dry run — report the rowcount that would be deleted without
    /// modifying any rows.
    #[arg(long)]
    pub pretend: bool,
}

#[async_trait]
impl TypedCommand for PruneArgs {
    async fn run(self) -> Result<(), FrameworkError> {
        let count = if let Some(name) = &self.model {
            match crate::eloquent::prune_one(name, self.pretend).await? {
                Some(n) => n,
                None => {
                    eprintln!("model:prune: no pruner registered for `{name}`");
                    return Ok(());
                }
            }
        } else if self.pretend {
            crate::eloquent::prune_all_dry().await?
        } else {
            crate::eloquent::prune_all().await?
        };

        if self.pretend {
            println!("model:prune: would prune {count} rows");
        } else {
            println!("model:prune: pruned {count} rows");
        }
        Ok(())
    }
}
