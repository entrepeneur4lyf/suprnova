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
/// The derived `Serialize` impl emits the data slice plus the offset/
/// counter fields:
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
///
/// This shape is **not** identical to Laravel's
/// `LengthAwarePaginator::toArray()` — Laravel additionally emits
/// `first_page_url`, `last_page_url`, `next_page_url`,
/// `prev_page_url`, and a `links` array of `{url, label, page,
/// active}` descriptors. Suprnova's URL generation lives on the
/// response-shape constructors that own URL context:
/// [`Inertia::paginate`](crate::inertia::Inertia::paginate) (Inertia
/// scroll metadata — page identifiers, not absolute URLs) and
/// [`Resource::paginated`](crate::resources::Resource::paginated)
/// (JSON:API `links.{self,first,last,prev,next}`). The raw `Serialize`
/// shape is for explicit-shape consumers (custom JSON envelopes, test
/// assertions, telemetry payloads) that don't need URL fields.
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

    /// Generate the URL for a specific page number by appending the page
    /// query parameter to the configured `path`. Falls back to a bare
    /// `?<page_name>=N` when no `path` is set. `page_name` defaults to
    /// `"page"` when unset.
    ///
    /// The separator is `&` when `path` already carries a query string and
    /// `?` otherwise, so a path like `/users?sort=name` yields
    /// `/users?sort=name&page=2` rather than a malformed double-`?`. The
    /// page parameter name is percent-encoded (the value is a numeric page),
    /// so a custom name with reserved characters can't corrupt the URL.
    pub fn url_for_page(&self, page: u64) -> String {
        let key = self.page_name.as_deref().unwrap_or("page");
        crate::pagination::build_query_url(self.path.as_deref(), key, &page.to_string())
    }

    /// `true` when there is a next page to fetch.
    pub fn has_more_pages(&self) -> bool {
        self.current_page < self.last_page
    }

    /// `true` when the paginator is on the first page (or before it,
    /// for a defensively low page value). Equivalent to Laravel's
    /// `AbstractPaginator::onFirstPage`.
    pub fn on_first_page(&self) -> bool {
        self.current_page <= 1
    }

    /// `true` when the paginator is on the last page (no further
    /// `?page=N` will yield rows). Equivalent to Laravel's
    /// `AbstractPaginator::onLastPage`.
    ///
    /// An empty result set (`total == 0`) returns `true` — there is
    /// no "next page" to fetch, which is what Laravel returns too
    /// (`onLastPage` ↔ `!hasMorePages`, and `hasMorePages` is `false`
    /// when `lastPage == 0`).
    pub fn on_last_page(&self) -> bool {
        !self.has_more_pages()
    }

    /// `true` when there are enough rows to span multiple pages.
    /// Equivalent to Laravel's `AbstractPaginator::hasPages`:
    /// either we're not on page 1 (so a previous page exists) or
    /// there are more pages to fetch.
    pub fn has_pages(&self) -> bool {
        self.current_page != 1 || self.has_more_pages()
    }

    /// `true` when the page slice contains no rows. Equivalent to
    /// Laravel's `AbstractPaginator::isEmpty`.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// `true` when the page slice contains at least one row.
    /// Equivalent to Laravel's `AbstractPaginator::isNotEmpty`.
    pub fn is_not_empty(&self) -> bool {
        !self.data.is_empty()
    }

    /// Number of rows on the current page slice. Equivalent to
    /// Laravel's `AbstractPaginator::count` (the `Countable`
    /// implementation). Not the total — for that use the `total`
    /// field directly.
    pub fn count(&self) -> usize {
        self.data.len()
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
    fn url_for_page_appends_to_existing_query_string() {
        // A base that already has a query string must get `&page=`, not a
        // second `?` — `/users?sort=name?page=2` is a malformed URL.
        let p = LengthAwarePaginator::new(vec![1, 2], 20, 10, 1).with_path("/users?sort=name");
        assert_eq!(p.url_for_page(2), "/users?sort=name&page=2");
    }

    #[test]
    fn url_for_page_encodes_the_page_parameter_name() {
        // A param name with characters that must be encoded never lands
        // raw in the URL (form-urlencoded renders a space as `+`).
        let p = LengthAwarePaginator::new(vec![1], 10, 10, 1)
            .with_path("/users")
            .with_page_name("weird key");
        assert_eq!(p.url_for_page(2), "/users?weird+key=2");
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

    #[test]
    fn on_first_page_predicate() {
        // page 1 → on first page, not on last
        let p = LengthAwarePaginator::new(vec![1; 10], 25, 10, 1);
        assert!(p.on_first_page());
        assert!(!p.on_last_page());
        assert!(p.has_pages());
        // page 2 → not on first, not on last (3 pages over 25 rows)
        let p = LengthAwarePaginator::new(vec![1; 10], 25, 10, 2);
        assert!(!p.on_first_page());
        assert!(!p.on_last_page());
        assert!(p.has_pages());
        // page 3 → not on first, on last
        let p = LengthAwarePaginator::new(vec![1; 5], 25, 10, 3);
        assert!(!p.on_first_page());
        assert!(p.on_last_page());
        assert!(p.has_pages());
    }

    #[test]
    fn empty_paginator_is_on_first_and_last_page_and_has_no_pages() {
        // total == 0 → last_page is 0; on_first_page is true (page 1
        // clamp); on_last_page is also true (no more pages); has_pages
        // is false (no need to render any page links).
        let p: LengthAwarePaginator<i32> = LengthAwarePaginator::new(vec![], 0, 10, 1);
        assert!(p.on_first_page());
        assert!(p.on_last_page());
        assert!(!p.has_pages());
        assert!(p.is_empty());
        assert!(!p.is_not_empty());
        assert_eq!(p.count(), 0);
    }

    #[test]
    fn single_page_has_no_extra_pages() {
        // 5 rows fits on a 10-per-page page → last_page = 1 → on first
        // AND on last AND no extra pages, but the slice is not empty.
        let p = LengthAwarePaginator::new(vec![1; 5], 5, 10, 1);
        assert!(p.on_first_page());
        assert!(p.on_last_page());
        assert!(!p.has_pages());
        assert!(!p.is_empty());
        assert!(p.is_not_empty());
        assert_eq!(p.count(), 5);
    }

    #[test]
    fn count_tracks_data_length_not_total() {
        // count() reports the page slice size, not the total row count.
        // Page 3 of 10/page over 25 rows holds only 5 rows.
        let p = LengthAwarePaginator::new(vec![1; 5], 25, 10, 3);
        assert_eq!(p.count(), 5);
        assert_eq!(p.total, 25);
    }
}
