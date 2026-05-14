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
}
