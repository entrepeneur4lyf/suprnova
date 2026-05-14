//! Pagination — `LengthAwarePaginator` (offset-based, knows total) and
//! `CursorPaginator` (keyset-based, encrypted cursors). The
//! [`Pagination`] facade wraps both over a SeaORM `Select<E>`.

pub mod cursor;
pub mod inertia;
pub mod length_aware;

pub use cursor::CursorPaginator;
pub use inertia::IntoInertiaScroll;
pub use length_aware::LengthAwarePaginator;

use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Select};

use crate::FrameworkError;

/// Static facade: `Pagination::length_aware` and `Pagination::cursor`.
pub struct Pagination;

impl Pagination {
    /// Run a length-aware (offset/limit + COUNT(*)) paginate against
    /// the configured `DB` connection.
    ///
    /// `current_page` is 1-based; values `< 1` are clamped to `1`.
    pub async fn length_aware<E>(
        query: Select<E>,
        per_page: u64,
        current_page: u64,
    ) -> Result<LengthAwarePaginator<E::Model>, FrameworkError>
    where
        E: EntityTrait,
        E::Model: Send + Sync,
    {
        let db = crate::DB::connection()?;
        let conn = db.inner();
        let page = current_page.max(1);

        let total = query.clone().count(conn).await?;
        let offset = (page - 1).saturating_mul(per_page);
        let data = query.offset(offset).limit(per_page).all(conn).await?;
        Ok(LengthAwarePaginator::new(data, total, per_page, page))
    }

    /// Run a cursor-based paginate. The cursor encodes the boundary
    /// value of `order_col` from the last row of the previous page;
    /// the next page is selected with `order_col > cursor` ordered
    /// ASC. `prev_cursor` is `None` in v1 (single-direction).
    ///
    /// `T::Model` must store the order column as a value that
    /// `to_string()`s into a stable cursor. The boundary value is
    /// re-parsed by SeaORM as the column's native type via the
    /// `serde_json::Value::String -> ColumnDef` coercion that SeaORM's
    /// `filter` does on string columns. For typed cursors over
    /// non-string columns, callers should compose the cursor outside
    /// this helper.
    pub async fn cursor<E, C>(
        query: Select<E>,
        cursor: Option<&str>,
        per_page: u64,
        order_col: C,
    ) -> Result<CursorPaginator<E::Model>, FrameworkError>
    where
        E: EntityTrait<Column = C>,
        E::Model: Send + Sync,
        C: ColumnTrait,
    {
        let db = crate::DB::connection()?;
        let conn = db.inner();

        let boundary: Option<String> = match cursor {
            Some(c) => Some(CursorPaginator::<E::Model>::decode_cursor(c)?),
            None => None,
        };

        let mut q = query.order_by_asc(order_col);
        if let Some(b) = &boundary {
            q = q.filter(order_col.gt(b.clone()));
        }
        // Fetch one extra row to detect whether there's a next page
        let mut rows = q.limit(per_page + 1).all(conn).await?;

        let next_cursor = if rows.len() as u64 > per_page {
            // The (per_page)-th row (0-indexed: index per_page-1) is the
            // last row of THIS page. The extra row past it tells us
            // there's more; cursor encodes the last *kept* row's column
            // value so the next request requests rows strictly greater.
            rows.truncate(per_page as usize);
            // SAFETY: rows.len() == per_page > 0
            let last = rows.last().unwrap();
            let value = sea_orm::ModelTrait::get(last, order_col);
            let boundary_string = value_to_cursor_string(&value);
            Some(CursorPaginator::<E::Model>::encode_cursor(&boundary_string))
        } else {
            None
        };

        Ok(CursorPaginator {
            data: rows,
            next_cursor,
            prev_cursor: None,
        })
    }
}

/// Convert a SeaORM dynamic `Value` to a string suitable for use as a
/// keyset cursor boundary. Only the simple-scalar variants are
/// supported; everything else is rendered via `Debug` as a last resort
/// (callers should not pass such columns).
fn value_to_cursor_string(v: &sea_orm::Value) -> String {
    use sea_orm::Value;
    match v {
        Value::String(Some(s)) => (**s).clone(),
        Value::String(None) => String::new(),
        Value::TinyInt(Some(i)) => i.to_string(),
        Value::SmallInt(Some(i)) => i.to_string(),
        Value::Int(Some(i)) => i.to_string(),
        Value::BigInt(Some(i)) => i.to_string(),
        Value::TinyUnsigned(Some(i)) => i.to_string(),
        Value::SmallUnsigned(Some(i)) => i.to_string(),
        Value::Unsigned(Some(i)) => i.to_string(),
        Value::BigUnsigned(Some(i)) => i.to_string(),
        Value::Float(Some(f)) => f.to_string(),
        Value::Double(Some(f)) => f.to_string(),
        Value::Bool(Some(b)) => b.to_string(),
        _ => format!("{:?}", v),
    }
}
