//! `Inertia` static facade — Laravel-style entrypoint for the most
//! common Inertia helpers.

use crate::pagination::IntoInertiaScroll;

use super::InertiaResponse;

/// Static facade. Today it exposes `Inertia::paginate`; future helpers
/// (render, location, etc.) will land here.
pub struct Inertia;

impl Inertia {
    /// Build an Inertia response with a single scroll-prop wired from
    /// a paginator.
    ///
    /// The metadata is taken from the paginator (page-name `"page"`
    /// for length-aware, `"cursor"` for cursor); the row vec is stored
    /// under `key`.
    pub fn paginate<T>(
        key: &'static str,
        paginator: impl IntoInertiaScroll<T>,
    ) -> InertiaResponse
    where
        T: serde::Serialize + 'static,
    {
        let (meta, data) = paginator.into_inertia_scroll();
        InertiaResponse::new(key).scroll(key, meta, data)
    }
}
