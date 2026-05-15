//! Length-aware paginator — knows the total row count, so it can
//! compute `last_page` and emit a numeric page UI.

use serde::Serialize;

/// Paginator that knows the total number of rows.
///
/// Equivalent to Laravel's `LengthAwarePaginator`. Returned by
/// [`Pagination::length_aware`](crate::pagination::Pagination::length_aware).
#[derive(Debug, Clone, Serialize)]
pub struct LengthAwarePaginator<T> {
    /// The rows on the current page.
    pub data: Vec<T>,
    /// Total row count across all pages.
    pub total: u64,
    /// Page size used to slice `total`.
    pub per_page: u64,
    /// 1-based current page index.
    pub current_page: u64,
    /// 1-based last page index. `0` when `total == 0`.
    pub last_page: u64,
    /// Base URL for generating page links (e.g. `/api/users`).
    /// When set, `url_for_page` appends `?page=N` to this value.
    /// `None` when constructed without a base URL (Inertia scroll usage).
    #[serde(skip)]
    pub base_url: Option<String>,
}

impl<T> LengthAwarePaginator<T> {
    /// Build a new paginator. Computes `last_page` from `total` and
    /// `per_page` (ceiling division). When `total == 0`, `last_page`
    /// is `0` and `current_page` is preserved as-is.
    pub fn new(data: Vec<T>, total: u64, per_page: u64, current_page: u64) -> Self {
        let last_page = if total == 0 || per_page == 0 {
            0
        } else {
            total.div_ceil(per_page)
        };
        Self {
            data,
            total,
            per_page,
            current_page,
            last_page,
            base_url: None,
        }
    }

    /// Set the base URL used by `url_for_page` to generate pagination links.
    /// Returns `self` for builder-style chaining.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Generate the URL for a specific page number by appending `?page=N`
    /// to the configured `base_url`. Falls back to `?page=N` when no
    /// `base_url` is set.
    pub fn url_for_page(&self, page: u64) -> String {
        match &self.base_url {
            Some(base) => format!("{}?page={}", base, page),
            None => format!("?page={}", page),
        }
    }

    /// `true` when there is a next page to fetch.
    pub fn has_more_pages(&self) -> bool {
        self.current_page < self.last_page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_more_pages() {
        let p = LengthAwarePaginator::new(vec![1, 2, 3], 25, 10, 2);
        assert_eq!(p.last_page, 3);
        assert!(p.has_more_pages());
    }

    #[test]
    fn last_page_no_more() {
        let p = LengthAwarePaginator::new(vec![1, 2, 3, 4, 5], 25, 10, 3);
        assert!(!p.has_more_pages());
    }

    #[test]
    fn total_zero_yields_empty_data() {
        let p: LengthAwarePaginator<i32> = LengthAwarePaginator::new(vec![], 0, 10, 1);
        assert_eq!(p.last_page, 0);
        assert!(!p.has_more_pages());
        assert!(p.data.is_empty());
    }

    #[test]
    fn url_for_page_with_base_url() {
        let p = LengthAwarePaginator::new(vec![1, 2], 20, 10, 1)
            .with_base_url("/api/users");
        assert_eq!(p.url_for_page(1), "/api/users?page=1");
        assert_eq!(p.url_for_page(2), "/api/users?page=2");
    }

    #[test]
    fn url_for_page_without_base_url_fallback() {
        let p = LengthAwarePaginator::new(vec![1], 10, 10, 1);
        assert_eq!(p.url_for_page(1), "?page=1");
    }
}
