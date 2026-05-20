//! `Collection<T>` — the thin Eloquent-style wrapper around `Vec<T>`.
//!
//! Phase 10A T7b ships only the constructor, `Deref` to `&[T]`, the
//! `From<Vec<T>>` bridge, and the trait derives needed by serde and
//! the `AsCollection` cast. Phase 10C fills in the full Laravel
//! method surface (`map`, `filter`, `pluck`, `groupBy`, `sortBy`, the
//! whole ~40-method chain — Laravel ships it at <https://laravel.com/docs/12.x/collections#available-methods>).
//!
//! `Deref<Target = [T]>` is the key insight: it makes every `&[T]`
//! method (`.len()`, `.iter()`, `.first()`, indexing, ...) work out of
//! the box without re-implementing them on the wrapper. The methods
//! that Phase 10C adds (`map`, `filter_by`, `group_by`, ...) are the
//! ones that don't already exist on slices or that need
//! self-by-value semantics for chainability.
//!
//! Phase 10B T9 adds [`Collection<M>::load`] / [`Collection::load_missing`]
//! on `Collection<M>` where `M` is a Suprnova model — these populate
//! the per-row `__eager` cache after the fact, mirroring Laravel's
//! `$collection->load(...)`. The bound is feature-gated on `M`
//! implementing `EagerLoadDispatch` (which `#[suprnova::model]` emits
//! automatically), so these methods only exist when the contained type
//! is a model.

use std::ops::Deref;

use serde::{Deserialize, Serialize};

use crate::eloquent::builder::EagerSpec;
use crate::eloquent::relations::eager::load_missing_path;
use crate::eloquent::relations::EagerLoadDispatch;
use crate::error::FrameworkError;

/// Thin wrapper around `Vec<T>`. Derefs to `&[T]` so all slice methods
/// (`.len()`, `.iter()`, `[index]`, ...) work without reimplementation;
/// extra Eloquent-style methods land in Phase 10C.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collection<T>(pub Vec<T>);

impl<T> Collection<T> {
    /// Construct an empty collection. Equivalent to `Vec::new()`.
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Consume the wrapper and return the inner `Vec<T>`.
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }

    /// Borrow the underlying slice. Equivalent to `&*collection` because
    /// of the `Deref<Target = [T]>` impl; provided as a named accessor
    /// for clarity at call sites.
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}

impl<T> Default for Collection<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> From<Vec<T>> for Collection<T> {
    fn from(v: Vec<T>) -> Self {
        Self(v)
    }
}

impl<T> AsRef<[T]> for Collection<T> {
    fn as_ref(&self) -> &[T] {
        &self.0
    }
}

impl<T> Deref for Collection<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl<T> IntoIterator for Collection<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// Eager-loading methods for `Collection<M>` when `M` is a Suprnova
/// model (`EagerLoadDispatch`). Loads relations on rows already in
/// memory — mirrors Laravel's `$collection->load([...])`.
///
/// These methods are also useful on plain `Vec<M>` via a `Collection`
/// wrap: `let mut c = Collection::from(rows); c.load(["posts"]).await?;`.
impl<M> Collection<M>
where
    M: EagerLoadDispatch + Send + Sync,
{
    /// Eager-load the named relations onto every row in the
    /// collection. Issues one query per top-level relation regardless
    /// of how many rows are loaded.
    ///
    /// Dotted paths (`"posts.comments"`) drive nested-path resolution
    /// — the same shape `Builder::with([...])` accepts.
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let mut users = User::all().await?.into();
    /// users.load(["posts.comments"]).await?;
    /// for u in users.iter() {
    ///     for p in u.posts_loaded() {
    ///         println!("{}: {} comments", p.title, p.comments_loaded().len());
    ///     }
    /// }
    /// ```
    pub async fn load<I, S>(&mut self, relations: I) -> Result<(), FrameworkError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let specs: Vec<EagerSpec> = relations
            .into_iter()
            .map(|s| EagerSpec::With(s.into()))
            .collect();
        if specs.is_empty() || self.0.is_empty() {
            return Ok(());
        }
        let db = crate::database::DB::connection()?;
        crate::eloquent::relations::eager::apply_eager_specs::<M>(&mut self.0, specs, db.inner())
            .await
    }

    /// Like [`Self::load`] but skip relations already populated on at
    /// least one row of the collection. Useful when you've fetched
    /// some users with `with(["posts"])` already and want to be sure
    /// without re-running the query.
    ///
    /// Dotted paths are recursive: if the head segment is cached on
    /// at least one row but the tail isn't (e.g. you previously
    /// called `with(["posts"])` and now want `load_missing(["posts.comments"])`),
    /// the orchestrator walks into each parent's cached children and
    /// loads only the missing tail. Conversely, if the entire path is
    /// already loaded on at least one row, the call is a no-op.
    ///
    /// Laravel's per-row semantics ("only load on the rows where the
    /// relation isn't there yet") aren't replicated in v1 — at each
    /// segment, v1 skips the whole relation if ANY row already has it
    /// cached. This is fine for the common pattern (you want every
    /// row to have it, don't pay twice if you already paid) and
    /// saves a per-row branch.
    pub async fn load_missing<I, S>(&mut self, relations: I) -> Result<(), FrameworkError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let paths: Vec<String> = relations.into_iter().map(|s| s.into()).collect();
        if paths.is_empty() || self.0.is_empty() {
            return Ok(());
        }
        let db = crate::database::DB::connection()?;
        for path in paths {
            load_missing_path::<M>(&mut self.0, &path, db.inner()).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        let c: Collection<i32> = Collection::new();
        assert!(c.is_empty());
    }

    #[test]
    fn from_vec_round_trips() {
        let v = vec![1, 2, 3];
        let c = Collection::from(v.clone());
        assert_eq!(c.as_slice(), &v[..]);
        assert_eq!(c.into_vec(), v);
    }

    #[test]
    fn deref_to_slice_provides_iter_and_indexing() {
        let c = Collection::from(vec!["a", "b", "c"]);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], "a");
        let collected: Vec<&&str> = c.iter().collect();
        assert_eq!(collected, vec![&"a", &"b", &"c"]);
    }

    #[test]
    fn into_iter_consumes_collection() {
        let c = Collection::from(vec![10, 20, 30]);
        let sum: i32 = c.into_iter().sum();
        assert_eq!(sum, 60);
    }
}
