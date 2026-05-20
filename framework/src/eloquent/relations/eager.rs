//! Eager-load orchestrator.
//!
//! Bridges [`Builder<M>`][crate::eloquent::Builder]'s recorded
//! `Vec<EagerSpec>` plan to the per-model `__eager_load` /
//! `__count_relation` / `__aggregate_relation` /
//! `__recurse_eager_load` dispatchers the
//! `#[suprnova::model]` macro emits.
//!
//! Phase 10B T9 ships:
//!
//! - [`apply_eager_specs`] — the top-level dispatcher. Walks each
//!   `EagerSpec` and routes to the right per-model entrypoint.
//! - Nested-path support via [`load_path`] — splits `"posts.comments"`
//!   into head + tail and asks the loaded children to recurse on the
//!   tail through their own `__recurse_eager_load` dispatcher.
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

use crate::eloquent::builder::EagerSpec;
use crate::eloquent::relations::{AggregateKind, EagerLoadDispatch};
use crate::error::FrameworkError;

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

/// Drive `__eager_load` for a single relation. Dotted paths
/// (`"posts.comments"`) load the head segment, then recurse into each
/// parent's loaded children via `__recurse_eager_load`.
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
        // before the recursion loop so the parents slice is free for
        // the per-row `__recurse_eager_load` calls.
        let mut refs: Vec<&mut M> = parents.iter_mut().collect();
        M::eager_load(head, refs.as_mut_slice(), db, None).await?;
    }

    if let Some(rest) = tail {
        // Recurse into each parent's loaded children. The macro-
        // emitted `__recurse_eager_load(head, rest, db)` looks up the
        // cached child rows on `self.__eager`, gets a `&mut [Child]`,
        // and recursively calls `Child::__eager_load(rest_head, ...)`
        // — peeling one segment per step.
        for p in parents.iter_mut() {
            p.recurse_eager_load(head, rest, db).await?;
        }
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
