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
    /// Laravel's per-row semantics ("only load on the rows where the
    /// relation isn't there yet") aren't replicated in v1 — v1 skips
    /// the whole relation if ANY row already has it cached. This is
    /// fine for the common pattern (you want every row to have it,
    /// don't pay twice if you already paid) and saves a per-row
    /// branch.
    pub async fn load_missing<I, S>(&mut self, relations: I) -> Result<(), FrameworkError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut specs: Vec<EagerSpec> = Vec::new();
        for rel in relations {
            let name = rel.into();
            // For nested paths, check the head segment only — that's
            // the cache cell `load(...)` populates at this level.
            let head = name.split_once('.').map(|(h, _)| h).unwrap_or(name.as_str());
            // Use `__eager.has(head)` via a free-standing helper on
            // each model. Models all share the same `__eager` field
            // shape so we walk every row and ask. If ANY row has the
            // cell populated, skip — Laravel's per-row mode is v2.
            let any_loaded = self.0.iter().any(|m| eager_has(m, head));
            if any_loaded {
                continue;
            }
            specs.push(EagerSpec::With(name));
        }
        if specs.is_empty() || self.0.is_empty() {
            return Ok(());
        }
        let db = crate::database::DB::connection()?;
        crate::eloquent::relations::eager::apply_eager_specs::<M>(&mut self.0, specs, db.inner())
            .await
    }
}

/// Probe a model's `__eager` cache for a given relation name without
/// going through the macro-emitted accessors (which panic on missing
/// relations). The macro auto-injects an `__eager: EagerLoadCache`
/// field on every model; this function reads that field via a
/// type-erased serialize path that avoids requiring `M: Model`.
///
/// Implementation: we serialize the model to JSON. The `__eager` cell
/// itself is skipped during serialization (the macro emits
/// `#[serde(skip)]` on `__eager`), so we can't ask it that way.
/// Instead we route through a hidden helper trait
/// [`crate::data::IsRelationLoaded`] that the macro implements per
/// model — but that's per-relation. The cheapest route is a
/// per-collection method bound to `EagerLoadDispatch`; since the
/// trait is sealed and the macro emits the impl, we add a
/// `has_eager(&self, name)` method via a new trait method.
///
/// For v1 we route through a dedicated trait method
/// [`EagerLoadDispatch::has_eager`] which the macro emits as a
/// one-liner against `self.__eager.has(name)`.
fn eager_has<M: EagerLoadDispatch>(m: &M, name: &str) -> bool {
    M::has_eager(m, name)
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
