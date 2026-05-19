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

use std::ops::Deref;

use serde::{Deserialize, Serialize};

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
