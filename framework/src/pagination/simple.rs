//! Phase 10C T7 — simple paginator (no count query).
//!
//! [`Paginator<T>`] is the cheap-to-compute sibling of
//! [`LengthAwarePaginator`][crate::pagination::LengthAwarePaginator]:
//! it skips the `COUNT(*)` query entirely and instead fetches
//! `per_page + 1` rows to detect whether a next page exists. Use it
//! for large tables where a total row count is too expensive — every
//! page costs one query instead of two.
//!
//! ## JSON shape
//!
//! Mirrors Laravel's `Paginator::toArray()`:
//!
//! ```json
//! {
//!   "data": [...],
//!   "current_page": 1,
//!   "per_page": 10,
//!   "has_more": true,
//!   "path": "/api/users"
//! }
//! ```
//!
//! `path` is omitted when unset.

use serde::Serialize;

/// Paginator without a total row count.
///
/// Equivalent to Laravel's `Paginator`. Returned by
/// [`Builder::simple_paginate`](crate::eloquent::Builder::simple_paginate).
#[derive(Debug, Clone, Serialize)]
pub struct Paginator<T> {
    /// The rows on the current page.
    pub data: Vec<T>,
    /// 1-based current page index.
    pub current_page: u64,
    /// Page size used to slice the underlying query.
    pub per_page: u64,
    /// `true` when there is at least one more row past this page.
    /// Computed by fetching `per_page + 1` rows and checking for the
    /// overflow.
    pub has_more: bool,
    /// Optional base URL — `path?page=N` is the typical URL shape
    /// clients build out of this paginator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl<T> Paginator<T> {
    /// Build a new simple paginator from its parts.
    pub fn new(data: Vec<T>, current_page: u64, per_page: u64, has_more: bool) -> Self {
        Self {
            data,
            current_page,
            per_page,
            has_more,
            path: None,
        }
    }

    /// Set the optional base URL. Returns `self` for builder-style
    /// chaining.
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_all_fields() {
        let p = Paginator::new(vec![1, 2, 3], 2, 10, true);
        assert_eq!(p.data.len(), 3);
        assert_eq!(p.current_page, 2);
        assert_eq!(p.per_page, 10);
        assert!(p.has_more);
        assert_eq!(p.path, None);
    }

    #[test]
    fn path_serializes_when_set() {
        let p = Paginator::new(vec![1, 2], 1, 10, true).with_path("/api/users");
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(
            json.get("path").and_then(|v| v.as_str()),
            Some("/api/users")
        );
    }

    #[test]
    fn path_omitted_when_unset() {
        let p = Paginator::new(vec![1, 2], 1, 10, true);
        let json = serde_json::to_value(&p).unwrap();
        assert!(json.get("path").is_none());
    }

    #[test]
    fn serializes_to_laravel_shape() {
        let p = Paginator::new(vec![10, 20, 30], 2, 10, false);
        let json = serde_json::to_value(&p).unwrap();
        let m = json.as_object().unwrap();
        assert!(m.contains_key("data"));
        assert!(m.contains_key("current_page"));
        assert!(m.contains_key("per_page"));
        assert!(m.contains_key("has_more"));
        assert_eq!(m.get("current_page").and_then(|v| v.as_u64()), Some(2));
        assert_eq!(m.get("per_page").and_then(|v| v.as_u64()), Some(10));
        assert_eq!(m.get("has_more").and_then(|v| v.as_bool()), Some(false));
    }
}
