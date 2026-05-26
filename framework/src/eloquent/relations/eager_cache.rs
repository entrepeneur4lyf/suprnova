//! Per-model eager-load cache.
//!
//! Auto-injected as `__eager: EagerLoadCache` on every
//! `#[suprnova::model]` struct by the macro. Stores eager-loaded
//! relation rows and aggregate values keyed by relation name; the
//! generated `<rel>_loaded()` / `<rel>_count()` accessors read from
//! it. Empty by default — populated by
//! [`Builder::with`][crate::eloquent::Builder::with] and related
//! eager-loading methods (T9).
//!
//! Stored values are boxed `Any` because each relation has its own
//! concrete type (`Vec<Post>`, `Option<Profile>`, `u64`, `f64`/`i64`
//! for aggregates). The accessor enforces type safety on read via
//! `Any::downcast_ref`.
//!
//! ## Clone semantics
//!
//! `EagerLoadCache` deep-clones — every cell carries a clone trampoline
//! (the [`ClonedBox`] struct) so cloning a model also clones any rows
//! the eager loader had attached. This matches Laravel's behaviour:
//! `clone $user` preserves `$user->posts`.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;

/// Eager-load cache. One per model instance.
///
/// Keyed by relation name (the `"posts"` in `with(["posts"])`). Holds
/// `Vec<T>` for HasMany / BelongsToMany kinds, `Option<T>` for HasOne /
/// BelongsTo, `u64` for `with_count` aggregates, and `f64`/`i64`/
/// arbitrary `T: Clone + Send + Sync` for `with_sum` / `with_avg` /
/// `with_min` / `with_max` aggregates.
#[derive(Default)]
pub struct EagerLoadCache {
    rows: HashMap<String, RelationCell>,
}

/// Internal storage variant. One per relation kind plus a generic
/// aggregate slot.
enum RelationCell {
    /// `Vec<T>` rows — populated by HasMany / BelongsToMany / Through /
    /// MorphMany / MorphToMany / MorphedByMany eager loaders.
    Many(ClonedBox),
    /// `Option<T>` row — populated by HasOne / BelongsTo / MorphTo /
    /// MorphOne / HasOneThrough eager loaders.
    One(ClonedBox),
    /// Plain count — populated by `with_count`.
    Count(u64),
    /// Aggregate value — populated by `with_sum` / `with_avg` /
    /// `with_min` / `with_max`. Stored type-erased so the same cell
    /// covers `f64` SUM/AVG and `i64` MIN/MAX without per-variant
    /// branching at the storage layer.
    Aggregate(ClonedBox),
}

impl EagerLoadCache {
    /// Construct an empty cache. Equivalent to `EagerLoadCache::default()`.
    pub fn new() -> Self {
        Self {
            rows: HashMap::new(),
        }
    }

    /// Whether this cache has a value for the given relation name.
    pub fn has(&self, name: &str) -> bool {
        self.rows.contains_key(name)
    }

    /// Store an eager-loaded HasMany / BelongsToMany row vector.
    pub fn set_many<T: Any + Clone + Send + Sync>(&mut self, name: &'static str, rows: Vec<T>) {
        self.rows
            .insert(name.to_string(), RelationCell::Many(ClonedBox::new(rows)));
    }

    /// Read an eager-loaded HasMany / BelongsToMany row vector.
    ///
    /// Panics with a clear message if the relation was not loaded —
    /// spec is explicit: silently returning `&[]` would hide bugs in
    /// user code that forgot `with([...])` and expected eager rows.
    ///
    /// Also panics if the cell exists but stores a different kind
    /// (HasOne / count / aggregate) — that combination indicates a
    /// framework bug rather than a user mistake.
    pub fn get_many<T: Any + Send + Sync>(&self, name: &str) -> &[T] {
        match self.rows.get(name) {
            Some(RelationCell::Many(boxed)) => boxed
                .downcast_ref::<Vec<T>>()
                .unwrap_or_else(|| {
                    panic!(
                        "eager-load cache: relation `{name}` was loaded but with a different \
                         type. Expected `{}`. This is a framework bug.",
                        std::any::type_name::<Vec<T>>(),
                    )
                })
                .as_slice(),
            Some(_) => panic!(
                "eager-load cache: relation `{name}` was loaded as a different kind (HasOne / \
                 count / aggregate), not HasMany / BelongsToMany.",
            ),
            None => panic!(
                "relation `{name}` was not eager-loaded; call `.with([\"{name}\"])` on the query \
                 builder before iterating",
            ),
        }
    }

    /// Mutably borrow the underlying `Vec<T>` for an eager-loaded
    /// HasMany / BelongsToMany cell. Used by T9's nested eager-load
    /// recursion: after the head segment populates `posts: Vec<Post>`
    /// on every user, the tail segment needs `&mut [Post]` to call
    /// `Post::__eager_load(...)` for the next path step.
    ///
    /// Returns `None` if the cell isn't populated, or if it stores a
    /// kind other than `Many`. The caller (the macro-emitted
    /// `__recurse_eager_load` arm) treats `None` as "nothing to
    /// recurse into" and exits cleanly.
    pub fn get_many_mut<T: Any + Send + Sync>(&mut self, name: &str) -> Option<&mut Vec<T>> {
        match self.rows.get_mut(name) {
            Some(RelationCell::Many(boxed)) => boxed.downcast_mut::<Vec<T>>(),
            _ => None,
        }
    }

    /// Mutably borrow the underlying `Option<T>` for an eager-loaded
    /// HasOne / BelongsTo / MorphOne / HasOneThrough cell. Used by
    /// T9's nested eager-load recursion to walk into a single-value
    /// child relation.
    ///
    /// Returns `None` if the cell isn't populated, was stored as a
    /// different kind, or contains a typed `None`. The caller treats
    /// `None` as "nothing to recurse into".
    pub fn get_one_mut<T: Any + Send + Sync>(&mut self, name: &str) -> Option<&mut T> {
        match self.rows.get_mut(name) {
            Some(RelationCell::One(boxed)) => {
                boxed.downcast_mut::<Option<T>>().and_then(|o| o.as_mut())
            }
            _ => None,
        }
    }

    /// Store an eager-loaded HasOne / BelongsTo row.
    pub fn set_one<T: Any + Clone + Send + Sync>(&mut self, name: &'static str, row: Option<T>) {
        self.rows
            .insert(name.to_string(), RelationCell::One(ClonedBox::new(row)));
    }

    /// Read an eager-loaded HasOne / BelongsTo row.
    ///
    /// Returns `None` if the relation was not loaded OR if the FK was
    /// null. Unlike `get_many`, this returns `None` instead of panicking
    /// for missing relations because callers like `<rel>_loaded()` on
    /// HasOne return `Option<&T>` — distinguishing "not loaded" from
    /// "loaded as None" would require a different accessor on the model.
    pub fn get_one<T: Any + Send + Sync>(&self, name: &str) -> Option<&T> {
        match self.rows.get(name) {
            Some(RelationCell::One(boxed)) => {
                boxed.downcast_ref::<Option<T>>().and_then(|o| o.as_ref())
            }
            Some(_) => panic!(
                "eager-load cache: relation `{name}` was loaded as a different kind, not HasOne \
                 / BelongsTo.",
            ),
            None => None,
        }
    }

    /// Store a `with_count` aggregate.
    pub fn set_count(&mut self, name: &'static str, count: u64) {
        self.rows
            .insert(name.to_string(), RelationCell::Count(count));
    }

    /// Read a `with_count` aggregate. Returns `None` if `with_count`
    /// wasn't called for this relation — callers like `<rel>_count()`
    /// turn that into a panic with a clear message.
    pub fn get_count(&self, name: &str) -> Option<u64> {
        match self.rows.get(name) {
            Some(RelationCell::Count(c)) => Some(*c),
            _ => None,
        }
    }

    /// Store a `with_sum` / `with_avg` / `with_min` / `with_max` value.
    ///
    /// The cache key is the wide `<rel>_<kind>_<col>` form built by
    /// [`aggregate_cache_key`][crate::eloquent::relations::aggregate_cache_key]
    /// — runtime-formatted, hence the `&str` (not `&'static str`)
    /// parameter. Multiple aggregates on the same relation (e.g.
    /// `with_sum(("posts","id"))` then `with_avg(("posts","id"))`)
    /// coexist without collision because the key encodes both the
    /// aggregate kind and the source column.
    pub fn set_aggregate<T: Any + Clone + Send + Sync>(&mut self, name: &str, value: T) {
        self.rows.insert(
            name.to_string(),
            RelationCell::Aggregate(ClonedBox::new(value)),
        );
    }

    /// Read an aggregate value of type `T`. Returns `None` if no
    /// aggregate was stored for this name OR if it was stored under a
    /// different type than `T`.
    pub fn get_aggregate<T: Any + Send + Sync>(&self, name: &str) -> Option<&T> {
        match self.rows.get(name) {
            Some(RelationCell::Aggregate(boxed)) => boxed.downcast_ref::<T>(),
            _ => None,
        }
    }
}

impl Clone for EagerLoadCache {
    fn clone(&self) -> Self {
        Self {
            rows: self
                .rows
                .iter()
                .map(|(k, v)| (k.clone(), v.clone_cell()))
                .collect(),
        }
    }
}

impl RelationCell {
    fn clone_cell(&self) -> RelationCell {
        match self {
            RelationCell::Count(c) => RelationCell::Count(*c),
            RelationCell::Many(inner) => RelationCell::Many(inner.clone()),
            RelationCell::One(inner) => RelationCell::One(inner.clone()),
            RelationCell::Aggregate(inner) => RelationCell::Aggregate(inner.clone()),
        }
    }
}

impl fmt::Debug for EagerLoadCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EagerLoadCache")
            .field("relations", &self.rows.keys().collect::<Vec<_>>())
            .finish()
    }
}

// ---- Clone-aware Any box -------------------------------------------------
//
// `Box<dyn Any>` isn't `Clone`, so cells storing one need a clone
// trampoline. We store the value-erased box alongside a function
// pointer that knows how to clone its concrete type. The function
// pointer's signature is fixed (`&dyn Any -> Box<dyn Any + Send + Sync>`)
// so it can sit in a struct field; the monomorphisation happens via a
// generic `ClonedBox::new<T>` that captures `T`'s `Clone` impl into a
// `fn` item.
//
// This avoids the `dyn Trait` + lifetime + upcasting puzzles that come
// with a single supertrait carrying both `Any` and `clone_dyn`. The
// `Any::downcast_ref::<T>` call on a plain `&dyn Any` is well-behaved.

struct ClonedBox {
    inner: Box<dyn Any + Send + Sync>,
    clone_fn: fn(&(dyn Any + Send + Sync)) -> Box<dyn Any + Send + Sync>,
}

impl ClonedBox {
    fn new<T: Any + Clone + Send + Sync>(value: T) -> Self {
        // Monomorphised per `T`: the function literal below captures
        // the concrete `T` type and the compiler emits a real fn that
        // downcasts, clones, and re-boxes. The fn pointer (not a
        // closure) keeps `ClonedBox: Copy`-friendly and dodges any
        // dyn-trait upcast machinery.
        fn clone_concrete<T: Any + Clone + Send + Sync>(
            v: &(dyn Any + Send + Sync),
        ) -> Box<dyn Any + Send + Sync> {
            let typed = v
                .downcast_ref::<T>()
                .expect("ClonedBox clone trampoline mismatched its own concrete type");
            Box::new(typed.clone())
        }
        Self {
            inner: Box::new(value),
            clone_fn: clone_concrete::<T>,
        }
    }

    fn downcast_ref<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.inner.downcast_ref::<T>()
    }

    fn downcast_mut<T: Any + Send + Sync>(&mut self) -> Option<&mut T> {
        self.inner.downcast_mut::<T>()
    }
}

impl Clone for ClonedBox {
    fn clone(&self) -> Self {
        Self {
            inner: (self.clone_fn)(self.inner.as_ref()),
            clone_fn: self.clone_fn,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct Row {
        id: i64,
    }

    #[test]
    fn set_many_get_many() {
        let mut c = EagerLoadCache::new();
        c.set_many("xs", vec![Row { id: 1 }, Row { id: 2 }]);
        assert_eq!(c.get_many::<Row>("xs").len(), 2);
    }

    #[test]
    fn set_one_get_one() {
        let mut c = EagerLoadCache::new();
        c.set_one("x", Some(Row { id: 3 }));
        assert_eq!(c.get_one::<Row>("x").unwrap().id, 3);
    }

    #[test]
    fn count_round_trip() {
        let mut c = EagerLoadCache::new();
        c.set_count("xs", 7);
        assert_eq!(c.get_count("xs"), Some(7));
    }

    #[test]
    fn aggregate_round_trip() {
        let mut c = EagerLoadCache::new();
        c.set_aggregate::<f64>("sum_amount", 12.5);
        assert_eq!(c.get_aggregate::<f64>("sum_amount"), Some(&12.5));
    }

    #[test]
    fn get_many_mut_allows_recursive_mutation() {
        let mut c = EagerLoadCache::new();
        c.set_many("xs", vec![Row { id: 1 }, Row { id: 2 }]);
        let v: &mut Vec<Row> = c.get_many_mut::<Row>("xs").expect("vec is present");
        v.push(Row { id: 3 });
        assert_eq!(c.get_many::<Row>("xs").len(), 3);
    }

    #[test]
    fn get_many_mut_returns_none_when_unset() {
        let mut c = EagerLoadCache::new();
        assert!(c.get_many_mut::<Row>("missing").is_none());
    }

    #[test]
    fn get_one_mut_allows_recursive_mutation() {
        let mut c = EagerLoadCache::new();
        c.set_one("x", Some(Row { id: 7 }));
        let r: &mut Row = c.get_one_mut::<Row>("x").expect("row is present");
        r.id = 99;
        assert_eq!(c.get_one::<Row>("x").unwrap().id, 99);
    }

    #[test]
    fn get_one_mut_returns_none_when_loaded_as_none() {
        let mut c = EagerLoadCache::new();
        c.set_one::<Row>("x", None);
        assert!(c.get_one_mut::<Row>("x").is_none());
    }
}
