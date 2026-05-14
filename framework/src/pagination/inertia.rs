//! Bridge from `LengthAwarePaginator` / `CursorPaginator` to Inertia's
//! `ScrollMetadata` — the protocol for infinite-scroll props.

use serde_json::Value;

use crate::inertia::ScrollMetadata;

use super::{CursorPaginator, LengthAwarePaginator};

/// Convert a paginator into an Inertia scroll prop: the metadata + the
/// row vec, which the caller wires onto an [`InertiaResponse`] under a
/// chosen key.
pub trait IntoInertiaScroll<T> {
    /// Split this paginator into its Inertia scroll metadata and the
    /// underlying data rows.
    fn into_inertia_scroll(self) -> (ScrollMetadata, Vec<T>);
}

impl<T> IntoInertiaScroll<T> for LengthAwarePaginator<T> {
    fn into_inertia_scroll(self) -> (ScrollMetadata, Vec<T>) {
        let mut meta = ScrollMetadata::new("page");
        meta.current_page = Some(Value::from(self.current_page as i64));
        if self.current_page > 1 {
            meta.previous_page = Some(Value::from((self.current_page - 1) as i64));
        }
        if self.has_more_pages() {
            meta.next_page = Some(Value::from((self.current_page + 1) as i64));
        }
        (meta, self.data)
    }
}

impl<T> IntoInertiaScroll<T> for CursorPaginator<T> {
    fn into_inertia_scroll(self) -> (ScrollMetadata, Vec<T>) {
        let mut meta = ScrollMetadata::new("cursor");
        meta.next_page = self.next_cursor.map(Value::String);
        meta.previous_page = self.prev_cursor.map(Value::String);
        (meta, self.data)
    }
}
