//! Prunable / MassPrunable — scheduled cleanup of stale rows.
//!
//! Two flavours of pruning:
//!
//! - [`Prunable`] — per-row prune. Iterates the scope, calls the
//!   optional [`Prunable::pruning`] hook for each row (audit / cleanup
//!   side effects), and force-deletes one row at a time. Use when
//!   per-row work is required.
//! - [`MassPrunable`] — set-based prune. Renders the scope's WHERE
//!   clause into a single `DELETE FROM ... WHERE ...` and lets the
//!   database drop every matching row in one statement. Use when no
//!   per-row hook is needed and rowcount could be large.
//!
//! The `#[suprnova::prunable]` attribute macro wraps the user's `impl
//! Prunable for T` (or `impl MassPrunable for T`) block, parses the
//! Self type, and submits a [`PrunerEntry`] into the inventory-backed
//! registry. The [`prune_all`] / [`prune_all_dry`] / [`prune_one`]
//! runners walk that registry; the `model:prune` console command
//! exposes the same surface on the CLI.
//!
//! ## Test isolation
//!
//! `inventory::iter::<PrunerEntry>()` is process-wide. Every
//! `#[prunable]` impl in a test binary is visible to every test, so
//! [`prune_all`] from a single test would invoke pruners for tables
//! the test didn't create. Test suites use [`prune_one`] for
//! per-pruner tests and one dedicated test for `prune_all` that sets
//! up every table; see `framework/tests/eloquent_soft_deletes.rs` for
//! the pattern.

use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;
use sea_orm::{EntityTrait, IntoActiveModel, PrimaryKeyTrait};
use serde::Serialize;

use crate::eloquent::{Builder, Model};
use crate::error::FrameworkError;

/// Per-row prune. Implementors define the `prunable()` scope returning
/// a [`Builder<Self>`] that selects rows to delete. The runner walks
/// the scope, calls [`Self::pruning`] for each row (optional hook for
/// audit logs / side effects), then force-deletes the row.
///
/// Pair with `#[suprnova::prunable]` on the impl block to register the
/// type into the inventory-backed pruner registry.
///
/// # Example
///
/// ```rust,ignore
/// use async_trait::async_trait;
/// use chrono::{Duration, Utc};
/// use suprnova::eloquent::Prunable;
///
/// #[suprnova::prunable]
/// #[async_trait]
/// impl Prunable for Session {
///     fn prunable() -> suprnova::Builder<Self> {
///         Self::query().filter_op(
///             "expires_at",
///             "<",
///             (Utc::now() - Duration::days(30)).to_rfc3339(),
///         )
///     }
/// }
/// ```
#[async_trait]
pub trait Prunable: Model + Sized + 'static
where
    Self: From<<Self::Entity as EntityTrait>::Model>,
    <Self::Entity as EntityTrait>::Model: From<Self>
        + IntoActiveModel<<Self::Entity as EntityTrait>::ActiveModel>
        + Serialize
        + Send
        + Sync,
    <Self::Entity as EntityTrait>::ActiveModel: Send,
    <<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Builder describing the rows that should be pruned. Each call
    /// constructs a fresh builder — implementors don't share state.
    fn prunable() -> Builder<Self>;

    /// Optional per-row hook fired right before the row is
    /// force-deleted. Use for audit logging, downstream notification,
    /// or any work that must observe the row before it disappears.
    /// Defaults to a no-op.
    async fn pruning(&self) -> Result<(), FrameworkError> {
        Ok(())
    }
}

/// Set-based prune. Implementors define the `prunable()` scope; the
/// runner renders its WHERE clause into a single `DELETE` and executes
/// it. No per-row hook — pair with [`Prunable`] (and pay the
/// row-by-row cost) when audit / notification work matters.
///
/// Pair with `#[suprnova::prunable]` on the impl block to register the
/// type into the inventory-backed pruner registry.
#[async_trait]
pub trait MassPrunable: Model + Sized + 'static
where
    Self: From<<Self::Entity as EntityTrait>::Model>,
    <Self::Entity as EntityTrait>::Model: From<Self>
        + IntoActiveModel<<Self::Entity as EntityTrait>::ActiveModel>
        + Serialize
        + Send
        + Sync,
    <Self::Entity as EntityTrait>::ActiveModel: Send,
    <<Self::Entity as EntityTrait>::PrimaryKey as PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Builder describing the rows that should be pruned. The runner
    /// only consumes its WHERE-side state; `select` / `order_by` /
    /// `group_by` / `having` / `limit` / `offset` / unions are
    /// ignored (the dedicated DELETE renderer walks `where_terms`
    /// only).
    fn prunable() -> Builder<Self>;
}

/// Type-erased pruner. Each `#[suprnova::prunable]` impl emits one of
/// these wrapping the type's runtime entry point. Stored inventory-
/// wide; iterated by [`prune_all`] / [`prune_all_dry`] /
/// [`prune_one`].
pub type PrunerFn =
    fn(dry_run: bool) -> Pin<Box<dyn Future<Output = Result<u64, FrameworkError>> + Send>>;

/// One row in the pruner registry. The macro submits these via
/// `inventory::submit!`.
#[derive(Debug, Clone, Copy)]
pub struct PrunerEntry {
    /// Last-segment type name (`"User"`, `"Session"`) — the same
    /// string the user passes to [`prune_one`] / `--model=...`.
    pub type_name: &'static str,
    /// Runtime entry point. `dry_run = true` returns the rowcount that
    /// would have been deleted without modifying any rows; `false`
    /// performs the delete.
    pub run: PrunerFn,
}

inventory::collect!(PrunerEntry);

/// Iterate every registered pruner. Public so the `model:prune`
/// console command can list registered types when `--model=...` names
/// an unknown one.
pub fn pruners() -> impl Iterator<Item = &'static PrunerEntry> {
    inventory::iter::<PrunerEntry>()
}

/// Run every registered pruner. Returns the total rowcount deleted
/// across all types. Errors propagate from the first failing pruner —
/// later pruners do not run.
pub async fn prune_all() -> Result<u64, FrameworkError> {
    let mut total = 0u64;
    for entry in pruners() {
        total += (entry.run)(false).await?;
    }
    Ok(total)
}

/// Dry-run companion to [`prune_all`]. Reports the rowcount that
/// would be deleted without modifying any rows.
pub async fn prune_all_dry() -> Result<u64, FrameworkError> {
    let mut total = 0u64;
    for entry in pruners() {
        total += (entry.run)(true).await?;
    }
    Ok(total)
}

/// Run a single registered pruner by type name. Returns
/// `Ok(Some(rowcount))` on success, `Ok(None)` when no pruner is
/// registered for the name, `Err(...)` when the pruner itself fails.
pub async fn prune_one(type_name: &str, dry_run: bool) -> Result<Option<u64>, FrameworkError> {
    for entry in pruners() {
        if entry.type_name == type_name {
            return Ok(Some((entry.run)(dry_run).await?));
        }
    }
    Ok(None)
}
