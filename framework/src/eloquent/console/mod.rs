//! Eloquent-side console commands.
//!
//! Each submodule registers a `CommandEntry` via the `#[command]` /
//! `#[derive(Command)]` macros — the inventory entries land at link
//! time, so the per-project `console` binary picks them up
//! automatically through `suprnova::console::dispatch_argv`.
//!
//! Currently:
//!
//! - [`prune`] — `model:prune` walks the [`PrunerEntry`] registry and
//!   force-deletes stale rows on every registered Prunable /
//!   MassPrunable type.
//!
//! [`PrunerEntry`]: crate::eloquent::PrunerEntry

pub mod prune;
