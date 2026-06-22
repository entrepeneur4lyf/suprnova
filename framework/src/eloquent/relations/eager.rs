//! Eager-load orchestrator.
//!
//! Bridges [`Builder<M>`][crate::eloquent::Builder]'s recorded
//! `Vec<EagerSpec>` plan to the per-model `__eager_load` /
//! `__count_relation` / `__aggregate_relation` /
//! `__recurse_eager_load_batched` dispatchers the
//! `#[suprnova::model]` macro emits.
//!
//! Phase 10B T9 ships:
//!
//! - [`apply_eager_specs`] — the top-level dispatcher. Walks each
//!   `EagerSpec` and routes to the right per-model entrypoint.
//! - Nested-path support via [`load_path`] — splits `"posts.comments"`
//!   into head + tail, then batches the tail across every parent via
//!   the `__recurse_eager_load_batched` dispatcher: all parents' cached
//!   children of the head are gathered into one slice and the next
//!   segment is loaded once, never once-per-parent (which would be N+1).
//! - `with_where` predicate routing via [`load_path_with_predicate`].
//!   The closure is type-erased here; downcast happens inside the
//!   per-relation dispatcher arm where the target type is statically
//!   known.
//!
//! ## Design notes
//!
//! - The orchestrator is module-private — users only see
//!   `Builder::get` / `Collection::load` / `Collection::load_missing`,
//!   which call into it.
//! - Errors propagate from the per-relation arms. The orchestrator
//!   does NOT swallow `FrameworkError::internal("no relation X")`;
//!   the user typo'ed a relation name, the error message names what
//!   went wrong.
//! - For nested-morph: the v1 contract is that `MorphTo` relations
//!   can NOT be the head of a nested path. The per-model
//!   `__recurse_eager_load` arm for any MorphTo relation returns a
//!   clear "nested morph recursion not supported in v1" error.

use std::any::Any;

use sea_orm::DatabaseConnection;

use crate::database::DbConnection;
use crate::database::transaction::ExecutorChoice;
use crate::eloquent::builder::EagerSpec;
use crate::eloquent::relations::{AggregateKind, EagerLoadDispatch};
use crate::error::FrameworkError;

/// Resolve a [`DbConnection`] to thread through the eager-load
/// dispatchers, honoring per-model / per-builder routing the same way
/// the parent SELECT did.
///
/// The trait-level `EagerLoadDispatch::eager_load` signature takes a
/// `&DatabaseConnection` argument that the macro-emitted leaf arms
/// largely ignore — every leaf re-resolves via
/// [`ExecutorChoice::resolve_read`] internally so ambient `CURRENT_TX`
/// (and the per-target-model default connection) take effect at the
/// SQL leaf. But the orchestrator still has to hand SOMETHING down.
/// Historically it called [`crate::DB::connection`] unconditionally,
/// which fails when an app registers only named / per-model
/// connections and never installed a default pool. Resolve via the
/// executor chain instead so the default pool isn't a hard
/// prerequisite for eager loading.
///
/// Inside a transaction the `Tx` variant doesn't produce a pool handle;
/// fall back to [`crate::DB::connection`] in that case. Being inside a
/// transaction implies the default pool exists (you opened the tx
/// against it), so this fallback never fires on the "no default pool"
/// configuration the new path is meant to support.
pub(crate) async fn resolve_eager_connection(
    tx_override: Option<&crate::database::TxHandle>,
    connection_override: Option<&str>,
    model_default_conn: Option<&'static str>,
) -> Result<DbConnection, FrameworkError> {
    let exec =
        ExecutorChoice::resolve_read(tx_override, connection_override, model_default_conn).await?;
    match exec {
        ExecutorChoice::Pool(conn, _) => Ok(conn),
        ExecutorChoice::Tx(_, _) => crate::database::DB::connection(),
    }
}

/// Walk an eager-load plan against a slice of parent rows. Each spec
/// dispatches into the appropriate per-model entrypoint; multi-spec
/// plans run sequentially because the dispatchers borrow `parents`
/// mutably.
///
/// The plan is consumed (`Vec<EagerSpec>` by value) because
/// `WithWhere`'s `Box<dyn Any>` predicate isn't `Clone` — re-running
/// the same plan would require boxing a closure copy at build time,
/// which Rust's type system doesn't help with for `FnOnce`. If a
/// caller needs to apply the same logical plan twice, build the spec
/// list twice.
pub(crate) async fn apply_eager_specs<M>(
    parents: &mut [M],
    specs: Vec<EagerSpec>,
    db: &DatabaseConnection,
) -> Result<(), FrameworkError>
where
    M: EagerLoadDispatch + Send + Sync,
{
    if parents.is_empty() {
        return Ok(());
    }

    for spec in specs {
        match spec {
            EagerSpec::With(path) => {
                load_path::<M>(parents, &path, db).await?;
            }
            EagerSpec::WithCount(rel) => {
                load_count::<M>(parents, &rel, db).await?;
            }
            EagerSpec::WithSum(rel, col) => {
                load_aggregate::<M>(parents, &rel, &col, AggregateKind::Sum, db).await?;
            }
            EagerSpec::WithAvg(rel, col) => {
                load_aggregate::<M>(parents, &rel, &col, AggregateKind::Avg, db).await?;
            }
            EagerSpec::WithMin(rel, col) => {
                load_aggregate::<M>(parents, &rel, &col, AggregateKind::Min, db).await?;
            }
            EagerSpec::WithMax(rel, col) => {
                load_aggregate::<M>(parents, &rel, &col, AggregateKind::Max, db).await?;
            }
            EagerSpec::WithWhere(rel, predicate) => {
                load_path_with_predicate::<M>(parents, &rel, predicate, db).await?;
            }
        }
    }
    Ok(())
}

/// Drive a single `load_missing` path. The contract mirrors Laravel's
/// `$collection->loadMissing(...)`: only fill the relations that
/// aren't already cached, evaluated per-row.
///
/// Path semantics:
///
/// - Flat name (`"posts"`): partition the parents into rows that
///   already have `posts` cached vs. rows that don't. The bulk-load
///   runs only against the needs-load subset; already-loaded rows
///   stay untouched.
/// - Dotted (`"posts.comments"`): partition by `has_eager(head)` too.
///     - Rows WITHOUT the head: load the FULL path against just that
///       subset (head + tail) — nothing's cached on these so a normal
///       eager-load drives the whole walk.
///     - Rows WITH the head cached: recurse into the cached children
///       with `missing_only = true` so the per-child partitioning
///       happens one level down (and again at every further level of a
///       longer dotted path).
///
/// The tail recursion is batched across every parent through
/// `__recurse_eager_load_batched`: all parents' cached children of the
/// head are gathered into one slice and the next segment is loaded once,
/// not once per parent (which would be N+1 on a deep path). The
/// `missing_only` flag propagates through the batched arm — it
/// partitions the combined children the same way before bulk-loading the
/// next segment. Flat `with(...)` always passes `missing_only = false`,
/// so the partition is a no-op there (bulk-loads everything every time).
pub(crate) async fn load_missing_path<M>(
    parents: &mut [M],
    path: &str,
    db: &DatabaseConnection,
) -> Result<(), FrameworkError>
where
    M: EagerLoadDispatch + Send + Sync,
{
    let (head, tail) = match path.split_once('.') {
        Some((h, t)) => (h, Some(t)),
        None => (path, None),
    };

    match tail {
        None => {
            // Flat path. Partition into rows that need `head` loaded
            // vs. rows that already have it. Bulk-load only the needs
            // subset; already-loaded rows are skipped.
            let mut needs: Vec<&mut M> =
                parents.iter_mut().filter(|p| !p.has_eager(head)).collect();
            if needs.is_empty() {
                return Ok(());
            }
            M::eager_load(head, needs.as_mut_slice(), db, None).await
        }
        Some(rest) => {
            // Dotted path. Walk the parents once to learn which need
            // the head loaded; bulk-load the head against just that
            // subset, then recurse the tail batched across EVERY parent
            // with `missing_only = true`. The per-child partition then
            // happens one level down inside the macro-emitted batched
            // recurse arm.
            //
            // The tail recursion runs on every row (needs-full and
            // has-head alike) because freshly-loaded children have
            // nothing cached at the tail level — the recursion's own
            // partition trivially loads all of them, symmetric with
            // the has-head branch where the partition filters out
            // already-cached tails.
            //
            // We do the head bulk-load in a scope so the
            // `Vec<&mut M>` borrow ends before the batched recursion
            // call. Otherwise Rust holds the slice's borrow across
            // the await and refuses the second `iter_mut()`.
            {
                let mut needs_head: Vec<&mut M> =
                    parents.iter_mut().filter(|p| !p.has_eager(head)).collect();
                if !needs_head.is_empty() {
                    M::eager_load(head, needs_head.as_mut_slice(), db, None).await?;
                }
            }
            // Recurse the tail batched across ALL parents (see
            // `load_path` for why per-parent recursion is N+1). The
            // batched arm gathers every parent's cached children of
            // `head` into one slice, loads the next segment once with
            // `missing_only = true` (so already-cached tails are
            // skipped), then recurses on the remainder.
            let mut refs: Vec<&mut M> = parents.iter_mut().collect();
            M::recurse_eager_load_batched(refs.as_mut_slice(), head, rest, db, true).await?;
            Ok(())
        }
    }
}

/// Drive `__eager_load` for a single relation. Dotted paths
/// (`"posts.comments"`) load the head segment across all parents, then
/// recurse into the loaded children batched across every parent via
/// `__recurse_eager_load_batched` so each nested segment is one query.
async fn load_path<M>(
    parents: &mut [M],
    path: &str,
    db: &DatabaseConnection,
) -> Result<(), FrameworkError>
where
    M: EagerLoadDispatch + Send + Sync,
{
    let (head, tail) = match path.split_once('.') {
        Some((h, t)) => (h, Some(t)),
        None => (path, None),
    };

    {
        // Borrow scope — the dispatcher's `&mut [&mut M]` shape
        // requires a fresh `Vec<&mut M>` per call. Drop the borrow
        // before the batched recursion so the parents slice is free for
        // the `__recurse_eager_load_batched` call below.
        let mut refs: Vec<&mut M> = parents.iter_mut().collect();
        M::eager_load(head, refs.as_mut_slice(), db, None).await?;
    }

    if let Some(rest) = tail {
        // Recurse into the loaded children, batched across ALL parents.
        // The macro-emitted `__recurse_eager_load_batched(parents, head,
        // rest, db, missing_only)` gathers every parent's cached
        // children of `head` into one combined `&mut [Child]` slice and
        // issues a SINGLE `Child::eager_load(rest_head, ...)` across the
        // lot, then recurses on the remaining tail. Doing this per
        // parent instead — one `eager_load` per parent — would re-issue
        // the next-segment query N times (one per parent), the classic
        // N+1. The `false` here means "always bulk-load each segment";
        // the `load_missing` orchestrator passes `true` to skip
        // already-cached segments.
        let mut refs: Vec<&mut M> = parents.iter_mut().collect();
        M::recurse_eager_load_batched(refs.as_mut_slice(), head, rest, db, false).await?;
    }
    Ok(())
}

/// Drive `__count_relation` for a single relation. No nested-path
/// support — `with_count(["posts.comments"])` would aggregate
/// `posts.comments` against `User`, which has no meaning in Eloquent
/// either. (`with_count(["posts"]).with(["posts.comments"])` is the
/// pattern users want; both specs work independently.)
async fn load_count<M>(
    parents: &mut [M],
    rel: &str,
    db: &DatabaseConnection,
) -> Result<(), FrameworkError>
where
    M: EagerLoadDispatch + Send + Sync,
{
    let mut refs: Vec<&mut M> = parents.iter_mut().collect();
    M::count_relation(rel, refs.as_mut_slice(), db).await
}

/// Drive `__aggregate_relation` for a single relation column.
async fn load_aggregate<M>(
    parents: &mut [M],
    rel: &str,
    col: &str,
    kind: AggregateKind,
    db: &DatabaseConnection,
) -> Result<(), FrameworkError>
where
    M: EagerLoadDispatch + Send + Sync,
{
    // `col` flows untyped from the public `with_sum`/`with_avg`/`with_min`/
    // `with_max` surface straight into `format!("SUM({col})", ...)` inside the
    // macro-emitted `aggregate_relation`. Unlike `Builder::sum`/`avg`/… (which
    // validate their column) and the rest of the builder, `Builder::validate_inputs`
    // never walks `eager_specs`, so this is the one aggregate path with no fence.
    // Validate the identifier here — the single chokepoint for every aggregate
    // kind and every model — so a crafted column can't break out of the call.
    crate::database::validate_identifier(col)?;
    let mut refs: Vec<&mut M> = parents.iter_mut().collect();
    M::aggregate_relation(rel, col, kind, refs.as_mut_slice(), db).await
}

/// Drive `__eager_load` with a type-erased predicate. The per-
/// relation dispatcher arm downcasts to the concrete
/// `Box<dyn FnOnce(Builder<R>) -> Builder<R>>` and applies before the
/// IN-query.
async fn load_path_with_predicate<M>(
    parents: &mut [M],
    rel: &str,
    predicate: Box<dyn Any + Send + Sync>,
    db: &DatabaseConnection,
) -> Result<(), FrameworkError>
where
    M: EagerLoadDispatch + Send + Sync,
{
    let mut refs: Vec<&mut M> = parents.iter_mut().collect();
    M::eager_load(rel, refs.as_mut_slice(), db, Some(predicate)).await
}
