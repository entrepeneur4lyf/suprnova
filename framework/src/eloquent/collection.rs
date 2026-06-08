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
use std::ops::{Deref, DerefMut};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::eloquent::builder::EagerSpec;
use crate::eloquent::model::Model;
use crate::eloquent::relations::EagerLoadDispatch;
use crate::eloquent::relations::eager::load_missing_path;
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

/// Mirrors `Vec<T>`'s `DerefMut<Target = [T]>` so call sites that
/// previously held a `&mut Vec<M>` from `Model::query().get()` keep
/// working unchanged after the T5b return-type sweep —
/// `.iter_mut()`, `.sort()`, slice-shape mutation all stay available.
/// Adding owned mutation (`.push`, `.pop`) still requires unwrapping
/// to `Vec` via [`Collection::into_vec`].
impl<T> DerefMut for Collection<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.0
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
///
/// The `M: Model` bound on this impl block is satisfied by every
/// `#[suprnova::model]` struct automatically (the macro emits both
/// `EagerLoadDispatch` and `Model` for every annotated type). Real
/// user code never picks up just `EagerLoadDispatch`. The bound is
/// required so [`Self::load`] can consult `M::default_connection_name()`
/// — eager loading must honour `#[model(connection = "...")]` routing
/// in the same way the parent `Builder::get` does.
impl<M> Collection<M>
where
    M: EagerLoadDispatch + Send + Sync + Model,
    M: From<<M::Entity as sea_orm::EntityTrait>::Model>,
    <M::Entity as sea_orm::EntityTrait>::Model: From<M>
        + sea_orm::IntoActiveModel<<M::Entity as sea_orm::EntityTrait>::ActiveModel>
        + serde::Serialize
        + Send
        + Sync,
    <M::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<M::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
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
        let db = crate::eloquent::relations::eager::resolve_eager_connection(
            None,
            None,
            M::default_connection_name(),
        )
        .await?;
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
        let db = crate::eloquent::relations::eager::resolve_eager_connection(
            None,
            None,
            M::default_connection_name(),
        )
        .await?;
        for path in paths {
            load_missing_path::<M>(&mut self.0, &path, db.inner()).await?;
        }
        Ok(())
    }
}

// ─── Phase 10C T5b: model-aware string-keyed Laravel surface ─────────
//
// String-keyed methods that route per-row field reads through the
// macro-emitted `Model::field_value(name)`. The `pluck_by` /
// `group_by_with` / `sort_with` / `key_by_with` family on the generic
// `impl<T>` block above takes closures and works for any `T`; the
// methods below take column-name strings and only exist when `M`
// implements `Model` (so the macro has emitted the `field_value`
// arms).
//
// Method matrix:
//
// - Lookup: `pluck("col")`, `pluck_keyed("k", "v")`,
//   `group_by("col")`, `key_by("col")`
// - Order: `sort_by("col")`, `sort_by_desc("col")`
// - Filter: `where_eq("col", v)`, `where_in("col", [v...])`,
//   `where_not_in("col", [v...])`
// - Aggregate: `sum::<T>("col")`, `avg::<T>("col")`,
//   `min::<T>("col")`, `max::<T>("col")`
// - Serialise: `to_array()`, `to_json()` (T5b stubs — T6 extends with
//   `hidden` / `visible` / `appends` filtering)
//
// Aggregates and `pluck` deserialise the JSON values via
// `serde_json::from_value` — rows whose field is missing or whose JSON
// doesn't round-trip into the target type are silently skipped. That
// matches Laravel's `$collection->pluck('col')` semantics where
// missing keys yield `null`s the caller is expected to handle.
//
// The `M: Model` bound has to re-elaborate `Model`'s own where-clause
// bounds because Rust's trait elaboration doesn't transitively
// propagate associated-type bounds from a supertrait's where clause
// to a subtrait's method bodies. Same pattern as `impl<M: Model>
// Builder<M>` in `builder.rs` and `FirstOrCreate` in `model.rs` — the
// methods below only call `m.field_value(name)` (the new T5b trait
// method) which by itself doesn't need every supertrait bound, but
// `Model`'s where clause re-elaboration is what makes `M: Model`
// resolvable at the impl block boundary.

impl<M> Collection<M>
where
    M: Model,
    M: From<<M::Entity as sea_orm::EntityTrait>::Model>,
    <M::Entity as sea_orm::EntityTrait>::Model: From<M>
        + sea_orm::IntoActiveModel<<M::Entity as sea_orm::EntityTrait>::ActiveModel>
        + serde::Serialize
        + Send
        + Sync,
    <M::Entity as sea_orm::EntityTrait>::ActiveModel: Send,
    <<M::Entity as sea_orm::EntityTrait>::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType:
        Send + Into<sea_orm::Value>,
{
    /// Project every row's value for `field` into a typed
    /// `Collection<U>`. Rows whose `field_value` returns `None`, or
    /// whose JSON value doesn't deserialise into `U`, are silently
    /// skipped (matches Laravel's missing-key handling).
    ///
    /// ## Example
    ///
    /// ```ignore
    /// let users: Collection<User> = User::query().get().await?;
    /// let names: Collection<String> = users.pluck::<String>("name");
    /// ```
    pub fn pluck<U>(&self, field: &str) -> Collection<U>
    where
        U: DeserializeOwned,
    {
        let mut out: Vec<U> = Vec::with_capacity(self.0.len());
        for m in &self.0 {
            if let Some(v) = m.field_value(field)
                && let Ok(u) = serde_json::from_value::<U>(v)
            {
                out.push(u);
            }
        }
        Collection(out)
    }

    /// Project every row into a `(K, V)` pair drawn from two columns
    /// and collect into a `HashMap<K, V>`. Later rows overwrite
    /// earlier ones for the same key.
    pub fn pluck_keyed<K, V>(&self, key_field: &str, value_field: &str) -> HashMap<K, V>
    where
        K: Eq + Hash + DeserializeOwned,
        V: DeserializeOwned,
    {
        let mut out: HashMap<K, V> = HashMap::new();
        for m in &self.0 {
            let (kv, vv) = match (m.field_value(key_field), m.field_value(value_field)) {
                (Some(k), Some(v)) => (k, v),
                _ => continue,
            };
            let k: K = match serde_json::from_value(kv) {
                Ok(x) => x,
                Err(_) => continue,
            };
            let v: V = match serde_json::from_value(vv) {
                Ok(x) => x,
                Err(_) => continue,
            };
            out.insert(k, v);
        }
        out
    }

    /// Bucket rows by `field` into `HashMap<String, Collection<M>>`.
    /// Keys are derived via `json_to_string_key`, which mirrors
    /// Laravel's `groupBy('team_id')` contract of string-keyed output
    /// regardless of the column's native type.
    pub fn group_by(&self, field: &str) -> HashMap<String, Collection<M>>
    where
        M: Clone,
    {
        let mut out: HashMap<String, Collection<M>> = HashMap::new();
        for m in &self.0 {
            let key = match m.field_value(field) {
                Some(v) => json_to_string_key(&v),
                None => continue,
            };
            out.entry(key).or_default().0.push(m.clone());
        }
        out
    }

    /// Index rows by `field` into `HashMap<String, M>`. Later
    /// duplicates overwrite earlier ones (matches Laravel's `keyBy`).
    /// Keys are stringified via `json_to_string_key`.
    pub fn key_by(&self, field: &str) -> HashMap<String, M>
    where
        M: Clone,
    {
        let mut out: HashMap<String, M> = HashMap::new();
        for m in &self.0 {
            let key = match m.field_value(field) {
                Some(v) => json_to_string_key(&v),
                None => continue,
            };
            out.insert(key, m.clone());
        }
        out
    }

    /// Sort rows ascending by `field`. Ordering is best-effort across
    /// JSON value shapes (see `compare_json`) — numeric, string,
    /// and boolean columns each sort cleanly within their own shape;
    /// heterogeneous mixes fall back to `Ordering::Equal`.
    ///
    /// Requires `M: Clone` because the implementation snapshots the
    /// underlying `Vec<M>` before delegating to `slice::sort_by` —
    /// the comparison closure borrows `&self.field_value(field)` via
    /// the contained `M`, while `sort_by` needs `&mut [M]`, so we
    /// can't sort in place against the original.
    pub fn sort_by(self, field: &str) -> Self
    where
        M: Clone,
    {
        let mut v = self.0.clone();
        v.sort_by(|a, b| compare_json(&a.field_value(field), &b.field_value(field)));
        Collection(v)
    }

    /// Sort rows descending by `field`. Sugar over
    /// [`Self::sort_by`] + `reverse`.
    pub fn sort_by_desc(self, field: &str) -> Self
    where
        M: Clone,
    {
        let mut s = self.sort_by(field);
        s.0.reverse();
        s
    }

    /// Keep rows where `field` equals `val` (JSON-value equality).
    /// Rows whose `field_value` returns `None` are dropped.
    pub fn where_eq(self, field: &str, val: Value) -> Self {
        Collection(
            self.0
                .into_iter()
                .filter(|m| m.field_value(field).as_ref() == Some(&val))
                .collect(),
        )
    }

    /// Keep rows where `field`'s value is in `vals`. Rows whose
    /// `field_value` returns `None` are dropped.
    pub fn where_in(self, field: &str, vals: Vec<Value>) -> Self {
        Collection(
            self.0
                .into_iter()
                .filter(|m| {
                    m.field_value(field)
                        .map(|v| vals.iter().any(|x| x == &v))
                        .unwrap_or(false)
                })
                .collect(),
        )
    }

    /// Keep rows where `field`'s value is NOT in `vals`. Rows whose
    /// `field_value` returns `None` are KEPT (the negation of the
    /// `where_in` predicate is: not present OR not in set).
    pub fn where_not_in(self, field: &str, vals: Vec<Value>) -> Self {
        Collection(
            self.0
                .into_iter()
                .filter(|m| {
                    m.field_value(field)
                        .map(|v| !vals.iter().any(|x| x == &v))
                        .unwrap_or(true)
                })
                .collect(),
        )
    }

    /// Sum the values of `field` across rows. Rows whose value is
    /// missing or doesn't deserialise into `T` are silently skipped.
    /// Empty result rolls down to `T::default()` through `Sum`'s
    /// identity element.
    pub fn sum<T>(&self, field: &str) -> T
    where
        T: DeserializeOwned + std::iter::Sum,
    {
        self.0
            .iter()
            .filter_map(|m| {
                m.field_value(field)
                    .and_then(|v| serde_json::from_value::<T>(v).ok())
            })
            .sum()
    }

    /// Mean of `field` across rows, as `f64`. Returns `None` when no
    /// row contributes a value (so the caller doesn't divide by zero).
    ///
    /// `T: Into<f64>` keeps the bound permissive: numeric columns
    /// (i64, f64, i32, ...) all coerce cleanly.
    pub fn avg<T>(&self, field: &str) -> Option<f64>
    where
        T: DeserializeOwned + Into<f64> + Copy,
    {
        let values: Vec<T> = self
            .0
            .iter()
            .filter_map(|m| {
                m.field_value(field)
                    .and_then(|v| serde_json::from_value::<T>(v).ok())
            })
            .collect();
        if values.is_empty() {
            return None;
        }
        let sum: f64 = values.iter().map(|v| (*v).into()).sum();
        Some(sum / values.len() as f64)
    }

    /// Smallest value of `field` across rows by `PartialOrd`. Returns
    /// `None` when no row contributes a value.
    pub fn min<T>(&self, field: &str) -> Option<T>
    where
        T: DeserializeOwned + PartialOrd,
    {
        let mut out: Option<T> = None;
        for m in &self.0 {
            let v: T = match m
                .field_value(field)
                .and_then(|x| serde_json::from_value::<T>(x).ok())
            {
                Some(v) => v,
                None => continue,
            };
            out = match out {
                None => Some(v),
                Some(cur) => Some(if v < cur { v } else { cur }),
            };
        }
        out
    }

    /// Largest value of `field` across rows by `PartialOrd`. Returns
    /// `None` when no row contributes a value.
    pub fn max<T>(&self, field: &str) -> Option<T>
    where
        T: DeserializeOwned + PartialOrd,
    {
        let mut out: Option<T> = None;
        for m in &self.0 {
            let v: T = match m
                .field_value(field)
                .and_then(|x| serde_json::from_value::<T>(x).ok())
            {
                Some(v) => v,
                None => continue,
            };
            out = match out {
                None => Some(v),
                Some(cur) => Some(if v > cur { v } else { cur }),
            };
        }
        out
    }

    /// Serialise the whole collection to a `serde_json::Value`. Phase
    /// 10C T6 — routes through each row's
    /// [`crate::eloquent::Model::to_array`] so the model's
    /// `hidden = [...]` / `visible = [...]` / `appends = [...]`
    /// filters propagate per-row when the collection serialises.
    ///
    /// The naive shape (`serde_json::to_value(&self.0)`) skips the
    /// per-model filters because serde's blanket `Serialize for Vec<T>`
    /// runs `T::serialize` directly, bypassing the trait-method
    /// override the macro emitted on `Model`.
    pub fn to_array(&self) -> Value {
        Value::Array(self.0.iter().map(|m| m.to_array()).collect())
    }

    /// Serialise the whole collection to a JSON string. Built on top
    /// of [`Self::to_array`] so the per-model filter pipeline applies
    /// to every row in the output.
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.to_array()).unwrap_or_default()
    }
}

/// Stringify a `serde_json::Value` for use as a `HashMap` key in
/// `group_by` / `key_by`. Matches Laravel's behaviour where
/// `groupBy('team_id')` yields string keys "1" / "2" regardless of
/// the column's native numeric type.
///
/// - `Value::String(s)` returns `s` (no quotes).
/// - `Value::Number(n)` / `Value::Bool(b)` / `Value::Null` use the
///   underlying primitive's `to_string`.
/// - Compound shapes (Object, Array) lean on `Value::to_string`
///   (JSON-encoded). Unusual but defined; users grouping by a JSON
///   column get the canonical encoding.
fn json_to_string_key(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Best-effort total order for sorting `Collection<M>` by a string
/// column name. Comparable JSON shapes (Number vs Number, String vs
/// String, Bool vs Bool) sort within their kind; heterogeneous mixes
/// fall back to `Ordering::Equal`. `None` sorts before any present
/// value (matches Postgres's default NULL FIRST for ASC).
fn compare_json(a: &Option<Value>, b: &Option<Value>) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
        (Some(Value::Number(x)), Some(Value::Number(y))) => x
            .as_f64()
            .partial_cmp(&y.as_f64())
            .unwrap_or(Ordering::Equal),
        (Some(Value::String(x)), Some(Value::String(y))) => x.cmp(y),
        (Some(Value::Bool(x)), Some(Value::Bool(y))) => x.cmp(y),
        _ => Ordering::Equal,
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
