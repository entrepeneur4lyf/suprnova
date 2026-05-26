//! Length-aware paginator — knows the total row count, so it can
//! compute `last_page` and emit a numeric page UI.

use serde::Serialize;

/// Paginator that knows the total number of rows.
///
/// Equivalent to Laravel's `LengthAwarePaginator`. Returned by
/// [`Pagination::length_aware`](crate::pagination::Pagination::length_aware)
/// and by [`Builder::paginate`](crate::eloquent::Builder::paginate).
///
/// ## JSON shape
///
/// Mirrors Laravel's `LengthAwarePaginator::toArray()` so it ships
/// directly to Inertia / JSON consumers:
///
/// ```json
/// {
///   "data": [...],
///   "current_page": 1,
///   "last_page": 3,
///   "per_page": 10,
///   "total": 25,
///   "from": 1,
///   "to": 10,
///   "path": "/api/users"
/// }
/// ```
///
/// `path` is omitted when unset.
#[derive(Debug, Clone, Serialize)]
pub struct LengthAwarePaginator<T> {
    /// The rows on the current page.
    pub data: Vec<T>,
    /// 1-based current page index.
    pub current_page: u64,
    /// 1-based last page index. `0` when `total == 0` (no rows means
    /// no last page); `1` when `total > 0` but fits on a single page.
    pub last_page: u64,
    /// Page size used to slice `total`.
    pub per_page: u64,
    /// Total row count across all pages.
    pub total: u64,
    /// 1-based index of the first row on this page. `None` when the
    /// page is empty.
    pub from: Option<u64>,
    /// 1-based index of the last row on this page. `None` when the
    /// page is empty.
    pub to: Option<u64>,
    /// Optional base URL for generating page links (e.g. `/api/users`).
    /// When `url_for_page` is called and `path` is set, the URL is
    /// `{path}?<page_name>=N`; otherwise it falls back to
    /// `?<page_name>=N`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Query-string parameter name used by [`Self::url_for_page`] when
    /// constructing page URLs. `None` resolves to `"page"`. Not
    /// serialized — clients receive `current_page` and reconstruct
    /// the URL on their side using whatever param name they've been
    /// instructed to use. Set automatically by
    /// [`Builder::paginate_using`](crate::eloquent::Builder::paginate_using)
    /// so `url_for_page` produces a URL with the same query key the
    /// paginator was driven from.
    #[serde(skip)]
    pub page_name: Option<String>,
}

impl<T> LengthAwarePaginator<T> {
    /// Build a new paginator from raw counts. Computes `last_page`,
    /// `from`, and `to` from the supplied `data` length, `total`,
    /// `per_page`, and `current_page`.
    ///
    /// When `total == 0` (or `per_page == 0`), `last_page` is `0` and
    /// both `from`/`to` are `None`. When `total > 0` and `data` is
    /// empty (e.g., the requested page is past the last page),
    /// `from`/`to` are still `None`.
    pub fn new(data: Vec<T>, total: u64, per_page: u64, current_page: u64) -> Self {
        let last_page = if total == 0 || per_page == 0 {
            0
        } else {
            total.div_ceil(per_page)
        };
        let (from, to) = if data.is_empty() || per_page == 0 {
            (None, None)
        } else {
            // 1-based offset of the first row on this page. The page
            // window is [(current_page - 1) * per_page + 1,
            // (current_page - 1) * per_page + data.len()].
            let base = current_page.saturating_sub(1).saturating_mul(per_page);
            (Some(base + 1), Some(base + data.len() as u64))
        };
        Self {
            data,
            total,
            per_page,
            current_page,
            last_page,
            from,
            to,
            path: None,
            page_name: None,
        }
    }

    /// Build a new paginator with `from`/`to` already known. Used by
    /// `Builder::paginate` when offset is known up-front.
    pub(crate) fn with_window(
        data: Vec<T>,
        total: u64,
        per_page: u64,
        current_page: u64,
        from: Option<u64>,
        to: Option<u64>,
    ) -> Self {
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
            from,
            to,
            path: None,
            page_name: None,
        }
    }

    /// Override the query-string parameter name used by
    /// [`Self::url_for_page`]. Call this when the paginator was driven
    /// from a non-default param name (e.g.
    /// `paginate_using("posts_page", 10)`) so generated URLs use the
    /// same key.
    pub fn with_page_name(mut self, name: impl Into<String>) -> Self {
        self.page_name = Some(name.into());
        self
    }

    /// Set the base URL used by `url_for_page` to generate pagination links.
    /// Returns `self` for builder-style chaining.
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// Set the base URL used by `url_for_page` to generate pagination links.
    /// Returns `self` for builder-style chaining.
    ///
    /// Legacy alias for [`Self::with_path`]; kept so existing callers
    /// using the pre-T7 API keep compiling.
    pub fn with_base_url(self, base_url: impl Into<String>) -> Self {
        self.with_path(base_url)
    }

    /// Generate the URL for a specific page number by appending
    /// `?<page_name>=N` to the configured `path`. Falls back to
    /// `?<page_name>=N` when no `path` is set. `page_name` defaults
    /// to `"page"` when unset.
    pub fn url_for_page(&self, page: u64) -> String {
        let key = self.page_name.as_deref().unwrap_or("page");
        match &self.path {
            Some(base) => format!("{}?{}={}", base, key, page),
            None => format!("?{}={}", key, page),
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
        assert_eq!(p.from, None);
        assert_eq!(p.to, None);
    }

    #[test]
    fn from_and_to_track_page_window() {
        // Page 2 of 10/page over 25 rows: rows 11..=20.
        let p = LengthAwarePaginator::new(vec![1; 10], 25, 10, 2);
        assert_eq!(p.from, Some(11));
        assert_eq!(p.to, Some(20));
    }

    #[test]
    fn from_and_to_on_final_short_page() {
        // Page 3 of 10/page over 25 rows: rows 21..=25 (only 5).
        let p = LengthAwarePaginator::new(vec![1; 5], 25, 10, 3);
        assert_eq!(p.from, Some(21));
        assert_eq!(p.to, Some(25));
    }

    #[test]
    fn url_for_page_with_path() {
        let p = LengthAwarePaginator::new(vec![1, 2], 20, 10, 1).with_path("/api/users");
        assert_eq!(p.url_for_page(1), "/api/users?page=1");
        assert_eq!(p.url_for_page(2), "/api/users?page=2");
    }

    #[test]
    fn url_for_page_with_base_url_alias_still_works() {
        let p = LengthAwarePaginator::new(vec![1, 2], 20, 10, 1).with_base_url("/api/users");
        assert_eq!(p.url_for_page(1), "/api/users?page=1");
    }

    #[test]
    fn url_for_page_without_path_fallback() {
        let p = LengthAwarePaginator::new(vec![1], 10, 10, 1);
        assert_eq!(p.url_for_page(1), "?page=1");
    }

    #[test]
    fn url_for_page_uses_custom_page_name() {
        let p = LengthAwarePaginator::new(vec![1, 2], 20, 10, 1)
            .with_path("/api/users")
            .with_page_name("posts_page");
        assert_eq!(p.url_for_page(2), "/api/users?posts_page=2");

        // No path — fallback also picks up the custom name.
        let p2 = LengthAwarePaginator::new(vec![1], 10, 10, 1).with_page_name("p");
        assert_eq!(p2.url_for_page(3), "?p=3");
    }

    #[test]
    fn page_name_not_serialized() {
        // `page_name` is `#[serde(skip)]` — clients receive
        // `current_page` and reconstruct the URL on their side.
        let p = LengthAwarePaginator::new(vec![1, 2], 20, 10, 1).with_page_name("posts_page");
        let json = serde_json::to_value(&p).unwrap();
        assert!(json.get("page_name").is_none());
    }

    #[test]
    fn path_serializes_when_set() {
        let p = LengthAwarePaginator::new(vec![1, 2], 20, 10, 1).with_path("/api/users");
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(
            json.get("path").and_then(|v| v.as_str()),
            Some("/api/users")
        );
    }

    #[test]
    fn path_omitted_when_unset() {
        let p = LengthAwarePaginator::new(vec![1, 2], 20, 10, 1);
        let json = serde_json::to_value(&p).unwrap();
        assert!(json.get("path").is_none());
    }
}
