//! Pagination — `LengthAwarePaginator` (offset-based, knows total) and
//! `CursorPaginator` (keyset-based, encrypted cursors). The
//! [`Pagination`] facade wraps both over a SeaORM `Select<E>`.

pub mod cursor;
pub mod inertia;
pub mod length_aware;
pub mod simple;

pub use cursor::{CursorDirection, CursorPaginator};
pub use inertia::IntoInertiaScroll;
pub use length_aware::LengthAwarePaginator;
pub use simple::Paginator;

use sea_orm::{ColumnTrait, EntityTrait, ModelTrait, QueryFilter, QueryOrder, QuerySelect, Select};

use crate::FrameworkError;
use crate::database::transaction::ExecutorChoice;

/// Static facade: `Pagination::length_aware` and `Pagination::cursor`.
pub struct Pagination;

impl Pagination {
    /// Run a length-aware (offset/limit + COUNT(*)) paginate.
    ///
    /// Routing matches the Eloquent builder's read path
    /// ([`ExecutorChoice::resolve_read`]): an ambient
    /// [`DB::transaction`](crate::DB::transaction) is honored (the COUNT and
    /// the page query both run on the transaction's connection), and a
    /// registered `__read_replica__` connection is used automatically.
    /// Use [`Self::length_aware_on`] to target a named connection.
    ///
    /// `current_page` is 1-based; values `< 1` are clamped to `1`.
    ///
    /// `per_page == 0` returns `FrameworkError::param("per_page")` (HTTP
    /// 400) — the same validation the Eloquent
    /// [`Builder::paginate`](crate::eloquent::Builder::paginate) enforces,
    /// so the two pagination surfaces agree on the zero-page-size contract
    /// instead of one silently emitting a `LIMIT 0` page.
    pub async fn length_aware<E>(
        query: Select<E>,
        per_page: u64,
        current_page: u64,
    ) -> Result<LengthAwarePaginator<E::Model>, FrameworkError>
    where
        E: EntityTrait,
        E::Model: Send + Sync,
    {
        if per_page == 0 {
            return Err(FrameworkError::param("per_page"));
        }
        let exec = ExecutorChoice::resolve_read(None, None, None).await?;
        Self::length_aware_with(exec, query, per_page, current_page).await
    }

    /// Run [`Self::length_aware`] against a specific named connection — the
    /// facade equivalent of [`Builder::on`](crate::eloquent::Builder::on).
    /// Routing matches the builder: an ambient `DB::transaction` still wins
    /// over the named connection, and the `__primary__` sentinel selects
    /// the default pool.
    pub async fn length_aware_on<E>(
        connection: &str,
        query: Select<E>,
        per_page: u64,
        current_page: u64,
    ) -> Result<LengthAwarePaginator<E::Model>, FrameworkError>
    where
        E: EntityTrait,
        E::Model: Send + Sync,
    {
        if per_page == 0 {
            return Err(FrameworkError::param("per_page"));
        }
        let exec = ExecutorChoice::resolve_read(None, Some(connection), None).await?;
        Self::length_aware_with(exec, query, per_page, current_page).await
    }

    async fn length_aware_with<E>(
        exec: ExecutorChoice,
        query: Select<E>,
        per_page: u64,
        current_page: u64,
    ) -> Result<LengthAwarePaginator<E::Model>, FrameworkError>
    where
        E: EntityTrait,
        E::Model: Send + Sync,
    {
        let page = current_page.max(1);
        let total = exec.select_count(query.clone()).await?;
        let offset = (page - 1).saturating_mul(per_page);
        let data = exec
            .select_all(query.offset(offset).limit(per_page))
            .await?;
        Ok(LengthAwarePaginator::new(data, total, per_page, page))
    }

    /// Run a cursor-based paginate.
    ///
    /// Routing matches the Eloquent builder's read path
    /// ([`ExecutorChoice::resolve_read`]): an ambient
    /// [`DB::transaction`](crate::DB::transaction) is honored and a
    /// registered `__read_replica__` connection is used automatically. Use
    /// [`Self::cursor_on`] to target a named connection.
    ///
    /// Cursors carry a typed [`sea_orm::Value`] of the `order_col`
    /// boundary plus a direction (`next`/`prev`). The cursor is opaque and
    /// always AES-256-GCM-encrypted via the process key ring; there is no
    /// plaintext base64 fallback — if encryption is not initialized,
    /// encoding returns an error rather than emitting a forgeable cursor.
    ///
    /// `per_page == 0` returns `FrameworkError::param("per_page")` (HTTP
    /// 400), matching the Eloquent
    /// [`Builder::cursor_paginate`](crate::eloquent::Builder::cursor_paginate)
    /// contract.
    ///
    /// # Behavior
    ///
    /// - `cursor == None`: first page. Returns the first `per_page`
    ///   rows ASC by `order_col`. `prev_cursor` is `None`; `next_cursor`
    ///   is set iff more rows remain.
    /// - `cursor == Some(<next>)`: forward step. Returns rows strictly
    ///   greater than the boundary, ASC. `prev_cursor` points back at
    ///   this page's first row; `next_cursor` is set iff more rows
    ///   remain.
    /// - `cursor == Some(<prev>)`: backward step. Returns rows strictly
    ///   less than the boundary, fetched DESC then reversed to ASC.
    ///   `prev_cursor` is set iff more rows lie before; `next_cursor`
    ///   points at this page's last row (back toward the caller's
    ///   origin).
    ///
    /// `order_col` should be a column with a total order suitable for
    /// keyset pagination — typically the primary key. Any SeaORM
    /// `Value` variant (`Int`, `BigInt`, `Uuid`, datetimes, decimals,
    /// strings, bytes, …) is supported; the dialect adapter binds the
    /// variant natively so Postgres / MySQL / SQLite all see the
    /// right SQL type.
    pub async fn cursor<E, C>(
        query: Select<E>,
        cursor: Option<&str>,
        per_page: u64,
        order_col: C,
    ) -> Result<CursorPaginator<E::Model>, FrameworkError>
    where
        E: EntityTrait<Column = C>,
        E::Model: Send + Sync,
        C: ColumnTrait + Copy,
    {
        if per_page == 0 {
            return Err(FrameworkError::param("per_page"));
        }
        let exec = ExecutorChoice::resolve_read(None, None, None).await?;
        Self::cursor_with(exec, query, cursor, per_page, order_col).await
    }

    /// Run [`Self::cursor`] against a specific named connection — the facade
    /// equivalent of [`Builder::on`](crate::eloquent::Builder::on). Routing
    /// matches the builder: an ambient `DB::transaction` still wins over the
    /// named connection, and the `__primary__` sentinel selects the default
    /// pool.
    #[allow(clippy::too_many_arguments)]
    pub async fn cursor_on<E, C>(
        connection: &str,
        query: Select<E>,
        cursor: Option<&str>,
        per_page: u64,
        order_col: C,
    ) -> Result<CursorPaginator<E::Model>, FrameworkError>
    where
        E: EntityTrait<Column = C>,
        E::Model: Send + Sync,
        C: ColumnTrait + Copy,
    {
        if per_page == 0 {
            return Err(FrameworkError::param("per_page"));
        }
        let exec = ExecutorChoice::resolve_read(None, Some(connection), None).await?;
        Self::cursor_with(exec, query, cursor, per_page, order_col).await
    }

    async fn cursor_with<E, C>(
        exec: ExecutorChoice,
        query: Select<E>,
        cursor: Option<&str>,
        per_page: u64,
        order_col: C,
    ) -> Result<CursorPaginator<E::Model>, FrameworkError>
    where
        E: EntityTrait<Column = C>,
        E::Model: Send + Sync,
        C: ColumnTrait + Copy,
    {
        let decoded = match cursor {
            Some(c) => Some(CursorPaginator::<E::Model>::decode_value(c)?),
            None => None,
        };

        let (rows, scan_direction) = match &decoded {
            None => {
                let rows = exec
                    .select_all(query.order_by_asc(order_col).limit(per_page + 1))
                    .await?;
                (rows, CursorDirection::Next)
            }
            Some((boundary, CursorDirection::Next)) => {
                let rows = exec
                    .select_all(
                        query
                            .order_by_asc(order_col)
                            .filter(order_col.gt(boundary.clone()))
                            .limit(per_page + 1),
                    )
                    .await?;
                (rows, CursorDirection::Next)
            }
            Some((boundary, CursorDirection::Prev)) => {
                let mut rows = exec
                    .select_all(
                        query
                            .order_by_desc(order_col)
                            .filter(order_col.lt(boundary.clone()))
                            .limit(per_page + 1),
                    )
                    .await?;
                rows.reverse();
                (rows, CursorDirection::Prev)
            }
        };

        // After the fetch:
        //   - Next scan: rows are ASC; overflow row (if any) is at END.
        //   - Prev scan: DESC-fetched then reversed → rows are ASC;
        //     overflow row (if any) is at START.
        let mut rows = rows;
        let overflow = rows.len() as u64 > per_page;
        if overflow {
            match scan_direction {
                CursorDirection::Next => {
                    rows.truncate(per_page as usize);
                }
                CursorDirection::Prev => {
                    let drop = rows.len() - per_page as usize;
                    rows.drain(0..drop);
                }
            }
        }

        let entered_via_next = matches!(decoded, Some((_, CursorDirection::Next)));
        let entered_via_prev = matches!(decoded, Some((_, CursorDirection::Prev)));

        // next_cursor: a forward cursor pinned at this page's last row.
        let next_cursor = {
            let has_next = match scan_direction {
                CursorDirection::Next => overflow,
                CursorDirection::Prev => true, // back-scan ⇒ we always came FROM further forward
            };
            if has_next && !rows.is_empty() {
                let last = rows.last().unwrap();
                let v = last.get(order_col);
                Some(CursorPaginator::<E::Model>::encode_value(
                    &v,
                    CursorDirection::Next,
                )?)
            } else {
                None
            }
        };

        // prev_cursor: a backward cursor pinned at this page's first row.
        let prev_cursor = {
            let has_prev = match scan_direction {
                CursorDirection::Next => entered_via_next || entered_via_prev,
                CursorDirection::Prev => overflow,
            };
            if has_prev && !rows.is_empty() {
                let first = rows.first().unwrap();
                let v = first.get(order_col);
                Some(CursorPaginator::<E::Model>::encode_value(
                    &v,
                    CursorDirection::Prev,
                )?)
            } else {
                None
            }
        };

        Ok(CursorPaginator::new(
            rows,
            per_page,
            next_cursor,
            prev_cursor,
        ))
    }
}

/// Append a single `key=value` query pair to an optional base path,
/// percent-encoding the pair. Shared by `LengthAwarePaginator::url_for_page`
/// (numeric `page=N`) and the cursor paginator's JSON:API link builder
/// (`cursor=<opaque>`), so both pick the separator the same way: `&` when
/// the base already carries a query string, `?` otherwise, and a bare
/// `?key=value` when there is no base path.
pub(crate) fn build_query_url(path: Option<&str>, key: &str, value: &str) -> String {
    let pair = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(key, value)
        .finish();
    match path {
        Some(base) => {
            let sep = if base.contains('?') { '&' } else { '?' };
            format!("{base}{sep}{pair}")
        }
        None => format!("?{pair}"),
    }
}

// ── Paginated<T> trait ────────────────────────────────────────────────────

/// Common surface consumed by `Resource::paginated` for building
/// JSON:API pagination links and meta. Implemented by
/// `LengthAwarePaginator<T>` and `CursorPaginator<T>`.
pub trait Paginated<T> {
    /// The items on the current page.
    fn items(&self) -> &[T];

    /// `meta.pagination` payload — conventionally placed under
    /// `meta.pagination` in JSON:API responses.
    fn meta_value(&self) -> serde_json::Value;

    /// Yield `(rel, href)` pairs for pagination links
    /// (`first`, `last`, `prev`, `next`, `self`).
    fn links_iter(&self) -> Box<dyn Iterator<Item = (&'static str, String)> + '_>;
}

impl<T> Paginated<T> for LengthAwarePaginator<T> {
    fn items(&self) -> &[T] {
        &self.data
    }

    fn meta_value(&self) -> serde_json::Value {
        serde_json::json!({
            "total": self.total,
            "per_page": self.per_page,
            "current_page": self.current_page,
            "last_page": self.last_page,
        })
    }

    fn links_iter(&self) -> Box<dyn Iterator<Item = (&'static str, String)> + '_> {
        let mut links: Vec<(&'static str, String)> = Vec::new();
        links.push(("self", self.url_for_page(self.current_page)));
        links.push(("first", self.url_for_page(1)));
        if self.last_page > 0 {
            links.push(("last", self.url_for_page(self.last_page)));
        }
        if self.current_page > 1 {
            links.push(("prev", self.url_for_page(self.current_page - 1)));
        }
        if self.current_page < self.last_page {
            links.push(("next", self.url_for_page(self.current_page + 1)));
        }
        Box::new(links.into_iter())
    }
}

impl<T> Paginated<T> for CursorPaginator<T> {
    fn items(&self) -> &[T] {
        &self.data
    }

    fn meta_value(&self) -> serde_json::Value {
        serde_json::json!({
            "next_cursor": self.next_cursor,
            "prev_cursor": self.prev_cursor,
        })
    }

    fn links_iter(&self) -> Box<dyn Iterator<Item = (&'static str, String)> + '_> {
        // Emit `next`/`prev` links from the stored cursor values, keyed by
        // `cursor_name` (defaulting to "cursor" — the query key
        // `Builder::cursor_paginate` reads). Mirrors the length-aware
        // paginator: links are produced whenever the corresponding cursor
        // exists, with or without a base path (no path → relative
        // `?cursor=<opaque>`).
        let key = self.cursor_name.as_deref().unwrap_or("cursor");
        let mut links: Vec<(&'static str, String)> = Vec::new();
        if let Some(next) = &self.next_cursor {
            links.push(("next", build_query_url(self.path.as_deref(), key, next)));
        }
        if let Some(prev) = &self.prev_cursor {
            links.push(("prev", build_query_url(self.path.as_deref(), key, prev)));
        }
        Box::new(links.into_iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cursor_with(next: Option<&str>, prev: Option<&str>) -> CursorPaginator<i32> {
        CursorPaginator::new(
            vec![1, 2],
            10,
            next.map(|s| s.to_string()),
            prev.map(|s| s.to_string()),
        )
    }

    fn links_of<T>(p: &impl Paginated<T>) -> HashMap<&'static str, String> {
        p.links_iter().collect()
    }

    #[test]
    fn cursor_links_emit_next_and_prev_with_path() {
        let p = cursor_with(Some("NEXTCUR"), Some("PREVCUR")).with_path("/api/items");
        let links = links_of(&p);
        assert_eq!(
            links.get("next").map(String::as_str),
            Some("/api/items?cursor=NEXTCUR")
        );
        assert_eq!(
            links.get("prev").map(String::as_str),
            Some("/api/items?cursor=PREVCUR")
        );
    }

    #[test]
    fn cursor_links_omit_absent_cursors() {
        // First page: prev_cursor is None → only a `next` link is emitted.
        let p = cursor_with(Some("NEXTCUR"), None).with_path("/api/items");
        let links = links_of(&p);
        assert!(links.contains_key("next"));
        assert!(!links.contains_key("prev"));
    }

    #[test]
    fn cursor_links_use_custom_cursor_name() {
        let p = cursor_with(Some("NEXTCUR"), None)
            .with_path("/api/items")
            .with_cursor_name("after");
        let links = links_of(&p);
        assert_eq!(
            links.get("next").map(String::as_str),
            Some("/api/items?after=NEXTCUR")
        );
    }

    #[test]
    fn cursor_links_append_to_existing_query_string() {
        // A base that already carries a query string must get `&cursor=`,
        // never a malformed second `?`.
        let p = cursor_with(Some("NEXTCUR"), None).with_path("/api/items?sort=name");
        let links = links_of(&p);
        assert_eq!(
            links.get("next").map(String::as_str),
            Some("/api/items?sort=name&cursor=NEXTCUR")
        );
    }

    #[test]
    fn cursor_links_without_path_are_relative() {
        // Parity with LengthAwarePaginator, which emits `?page=N` when no
        // base path is set.
        let p = cursor_with(Some("NEXTCUR"), None);
        let links = links_of(&p);
        assert_eq!(
            links.get("next").map(String::as_str),
            Some("?cursor=NEXTCUR")
        );
    }
}
