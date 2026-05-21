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

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
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

    // ─── Phase 10C T5a: generic Laravel surface ────────────────────────
    //
    // ~25 methods that work for any `T`. Model-aware methods that need
    // string-keyed field access (`pluck("name")`, `sum("price")`, ...)
    // ship in T5b on top of this surface — they require the
    // macro-emitted `Model::field_value` accessor.

    /// Construct from an owned `Vec<T>`. Equivalent to `Self::from(v)`
    /// but reads more naturally at call sites that want the explicit
    /// constructor instead of `.into()`.
    pub fn from_vec(v: Vec<T>) -> Self {
        Self(v)
    }

    /// Number of items. Provided inherently for ergonomic chaining;
    /// the `Deref<Target = [T]>` impl also exposes this from the slice.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when the collection has no items.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `true` when the collection has at least one item. Laravel
    /// parity (`$c->isNotEmpty()`); the inverse of [`Self::is_empty`].
    pub fn is_not_empty(&self) -> bool {
        !self.0.is_empty()
    }

    /// Borrow the first item, or `None` if empty.
    pub fn first(&self) -> Option<&T> {
        self.0.first()
    }

    /// Borrow the last item, or `None` if empty.
    pub fn last(&self) -> Option<&T> {
        self.0.last()
    }

    /// Borrow the first item that satisfies `pred`, or `None` if none
    /// match. Laravel: `$c->first(fn ($x) => ...)`.
    pub fn first_where<F>(&self, pred: F) -> Option<&T>
    where
        F: Fn(&&T) -> bool,
    {
        self.0.iter().find(pred)
    }

    /// Borrow the last item that satisfies `pred`, or `None` if none
    /// match. Laravel: `$c->last(fn ($x) => ...)`.
    pub fn last_where<F>(&self, pred: F) -> Option<&T>
    where
        F: Fn(&&T) -> bool,
    {
        self.0.iter().rev().find(pred)
    }

    /// Invoke `f` for every item by reference, then return the
    /// collection for chaining.
    ///
    /// Diverges from Laravel's PHP `each()` (which returns the
    /// collection but doesn't enable chaining the same way) — in Rust
    /// we move `self` through, which lets `c.each(...).map(...)`
    /// stay fluent without an interim binding. The closure receives
    /// `&T`, so it does not consume the items.
    pub fn each<F>(self, mut f: F) -> Self
    where
        F: FnMut(&T),
    {
        for t in &self.0 {
            f(t);
        }
        self
    }

    /// Transform every item with `f`, producing a `Collection<U>`.
    pub fn map<U, F>(self, f: F) -> Collection<U>
    where
        F: FnMut(T) -> U,
    {
        Collection(self.0.into_iter().map(f).collect())
    }

    /// Project every item into a `(K, V)` pair and collect into a
    /// `HashMap<K, V>`. Laravel's `mapWithKeys`.
    pub fn map_to_map<K, V, F>(self, f: F) -> HashMap<K, V>
    where
        K: Eq + Hash,
        F: FnMut(T) -> (K, V),
    {
        self.0.into_iter().map(f).collect()
    }

    /// Keep items for which `pred` is `true`.
    pub fn filter<F>(self, pred: F) -> Self
    where
        F: FnMut(&T) -> bool,
    {
        Collection(self.0.into_iter().filter(pred).collect())
    }

    /// Drop items for which `pred` is `true` — the inverse of
    /// [`Self::filter`]. Laravel's `reject`.
    pub fn reject<F>(self, mut pred: F) -> Self
    where
        F: FnMut(&T) -> bool,
    {
        Collection(self.0.into_iter().filter(|t| !pred(t)).collect())
    }

    /// Fold every item into an accumulator. Laravel's `reduce`.
    pub fn reduce<U, F>(self, initial: U, f: F) -> U
    where
        F: FnMut(U, T) -> U,
    {
        self.0.into_iter().fold(initial, f)
    }

    /// Bucket items by a closure-derived key into
    /// `HashMap<K, Collection<T>>`. Laravel's `groupBy(fn)`. The
    /// string-keyed `group_by("column")` overload lives in T5b.
    pub fn group_by_with<K, F>(self, mut key: F) -> HashMap<K, Collection<T>>
    where
        K: Eq + Hash,
        F: FnMut(&T) -> K,
    {
        let mut out: HashMap<K, Collection<T>> = HashMap::new();
        for t in self.0 {
            let k = key(&t);
            out.entry(k).or_default().0.push(t);
        }
        out
    }

    /// Index items by a closure-derived key into `HashMap<K, T>`.
    /// Later duplicates overwrite earlier ones (matches Laravel's
    /// `keyBy(fn)`). The string-keyed `key_by("column")` overload
    /// lives in T5b.
    pub fn key_by_with<K, F>(self, mut key: F) -> HashMap<K, T>
    where
        K: Eq + Hash,
        F: FnMut(&T) -> K,
    {
        self.0
            .into_iter()
            .map(|t| {
                let k = key(&t);
                (k, t)
            })
            .collect()
    }

    /// Extract a value from every item by reference, producing a
    /// `Collection<U>`. The collection is borrowed (`&self`), so the
    /// caller keeps ownership. Laravel's column-name `pluck("name")`
    /// overload lives in T5b.
    pub fn pluck_by<U, F>(&self, extract: F) -> Collection<U>
    where
        F: FnMut(&T) -> U,
    {
        Collection(self.0.iter().map(extract).collect())
    }

    /// Sort in place with `cmp`, then return self. Laravel's `sort(fn)`
    /// / `sortBy(fn)`. String-keyed `sort_by("column")` lives in T5b.
    pub fn sort_with<F>(mut self, cmp: F) -> Self
    where
        F: FnMut(&T, &T) -> std::cmp::Ordering,
    {
        self.0.sort_by(cmp);
        self
    }

    /// Drop duplicate items, keeping the first occurrence. Requires
    /// `T: Eq + Hash + Clone` because hashing into the seen-set needs
    /// an owned copy.
    pub fn unique(self) -> Self
    where
        T: Eq + Hash + Clone,
    {
        let mut seen: HashSet<T> = HashSet::new();
        Collection(
            self.0
                .into_iter()
                .filter(|t| seen.insert(t.clone()))
                .collect(),
        )
    }

    /// Drop duplicate items by closure-derived key. Only the key has
    /// to be `Eq + Hash` — items themselves can be anything.
    pub fn unique_by<K, F>(self, mut key: F) -> Self
    where
        K: Eq + Hash,
        F: FnMut(&T) -> K,
    {
        let mut seen: HashSet<K> = HashSet::new();
        Collection(self.0.into_iter().filter(|t| seen.insert(key(t))).collect())
    }

    /// Return `true` when any item satisfies `pred`. Laravel's
    /// `contains(fn)`. The value-equality overload (`contains(value)`)
    /// is naturally `c.iter().any(|x| x == &v)` via `Deref`.
    pub fn contains_where<F>(&self, pred: F) -> bool
    where
        F: FnMut(&T) -> bool,
    {
        self.0.iter().any(pred)
    }

    /// Split into batches of `n` items, returning a `Vec<Collection<T>>`.
    /// The final batch may be shorter than `n`. `n == 0` yields an
    /// empty `Vec`. Requires `T: Clone` because slice chunks must be
    /// cloned into owned `Collection<T>` batches.
    pub fn chunk(self, n: usize) -> Vec<Collection<T>>
    where
        T: Clone,
    {
        if n == 0 {
            return Vec::new();
        }
        self.0
            .chunks(n)
            .map(|slice| Collection(slice.to_vec()))
            .collect()
    }

    /// Take the first `n` items. If `n` exceeds the length, the whole
    /// collection is returned.
    pub fn take(self, n: usize) -> Self {
        Collection(self.0.into_iter().take(n).collect())
    }

    /// Drop the first `n` items. If `n` exceeds the length, an empty
    /// collection is returned.
    pub fn skip(self, n: usize) -> Self {
        Collection(self.0.into_iter().skip(n).collect())
    }

    /// Slice out `len` items starting at `start`. Both bounds are
    /// saturating — going past the end yields whatever's left.
    pub fn slice(self, start: usize, len: usize) -> Self {
        Collection(self.0.into_iter().skip(start).take(len).collect())
    }

    /// Reverse in place, returning self.
    pub fn reverse(mut self) -> Self {
        self.0.reverse();
        self
    }

    /// Shuffle in place with the thread-local RNG (`rand::rng()`).
    pub fn shuffle(mut self) -> Self {
        use rand::seq::SliceRandom;
        self.0.shuffle(&mut rand::rng());
        self
    }

    /// Pick one uniformly-random item by reference, or `None` if the
    /// collection is empty. Uses `rand::rng()`.
    pub fn random(&self) -> Option<&T> {
        use rand::seq::IndexedRandom;
        self.0.choose(&mut rand::rng())
    }

    /// Pick `n` uniformly-random items, returning a new collection.
    /// If `n` exceeds the length, the full collection is shuffled.
    /// Requires `T: Clone` because picks are taken out of a working
    /// copy.
    pub fn random_n(self, n: usize) -> Self
    where
        T: Clone,
    {
        use rand::seq::SliceRandom;
        let mut rng = rand::rng();
        let mut copy = self.0.clone();
        copy.shuffle(&mut rng);
        Collection(copy.into_iter().take(n).collect())
    }

    /// Append `other`'s items to self.
    pub fn concat(mut self, other: Self) -> Self {
        self.0.extend(other.0);
        self
    }

    /// Alias of [`Self::concat`] — Laravel ships both names.
    pub fn merge(self, other: Self) -> Self {
        self.concat(other)
    }

    /// Keep items in self that are NOT in `other`. `O(n*m)` — pick a
    /// hashed variant if performance matters.
    pub fn diff(self, other: Self) -> Self
    where
        T: PartialEq,
    {
        Collection(
            self.0
                .into_iter()
                .filter(|t| !other.0.iter().any(|o| o == t))
                .collect(),
        )
    }

    /// Keep items in self that ARE also in `other`. `O(n*m)` — pick a
    /// hashed variant if performance matters.
    pub fn intersect(self, other: Self) -> Self
    where
        T: PartialEq,
    {
        Collection(
            self.0
                .into_iter()
                .filter(|t| other.0.iter().any(|o| o == t))
                .collect(),
        )
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

impl<'a, T> IntoIterator for &'a Collection<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
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

    /// Like [`Self::load`] but evaluate the cache per row, not per
    /// collection. Each row is partitioned independently: rows that
    /// already have the relation cached stay untouched, rows that
    /// don't get the relation loaded. Mirrors Laravel's
    /// `$collection->loadMissing(...)` semantics.
    ///
    /// Dotted paths partition at every level. For
    /// `load_missing(["posts.comments"])`:
    ///
    /// - Rows without `posts` cached get the FULL path loaded
    ///   (`posts` and their `comments`).
    /// - Rows WITH `posts` already cached recurse into the cached
    ///   posts and load `comments` only on the posts that don't
    ///   already have comments cached.
    ///
    /// The same per-row partition repeats at every segment of a
    /// longer dotted path (`"posts.comments.author"` etc.) — at each
    /// step only the rows missing that segment get the bulk-load.
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
