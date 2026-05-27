//! `Inertia` static facade — Laravel-style entrypoint for the most
//! common Inertia helpers.

use crate::FrameworkError;
use crate::pagination::IntoInertiaScroll;

use super::config::InertiaConfig;
use super::response::IntoInertiaData;
use super::{Inertia303Middleware, InertiaResponse, InertiaVersionMiddleware};

/// Static facade. Today it exposes `Inertia::paginate`; future helpers
/// (render, location, etc.) will land here.
pub struct Inertia;

impl Inertia {
    /// Build an Inertia response with a single scroll-prop wired from
    /// a paginator.
    ///
    /// - `component` — the Inertia page component name (e.g. `"Users/Index"`).
    ///   This is what the frontend resolves to a real component.
    /// - `key` — the prop name under which the paginated rows land
    ///   (e.g. `"users"`). Scroll metadata is attached to the same key.
    ///
    /// The metadata page-name comes from the paginator itself:
    /// `"page"` for `LengthAwarePaginator`, `"cursor"` for
    /// `CursorPaginator`.
    pub fn paginate<T>(
        component: &'static str,
        key: &'static str,
        paginator: impl IntoInertiaScroll<T>,
    ) -> InertiaResponse
    where
        T: serde::Serialize + 'static,
    {
        let (meta, data) = paginator.into_inertia_scroll();
        InertiaResponse::new(component).scroll(key, meta, data)
    }

    /// Build an Inertia response from a `#[derive(Data)]` DTO.
    ///
    /// Lazy fields registered via `#[data(lazy)]` / `#[data(auto_lazy)]`
    /// resolve against the request's `?include=` set; the per-DTO allowlist
    /// enforces default-deny — disallowed includes return 400.
    pub fn data<T>(component: &'static str, dto: T) -> InertiaResponse
    where
        T: IntoInertiaData,
    {
        InertiaResponse::from_data_props(component, dto.__into_inertia_props())
    }

    /// Fallible sibling of [`data`](Self::data): returns
    /// `Err(FrameworkError)` (naming the offending field) if a DTO field's
    /// `Serialize` impl fails, instead of panicking.
    ///
    /// On the HTTP request path the panicking [`data`](Self::data) is fine —
    /// the panic-recovery middleware converts it to a 500. Prefer `try_data`
    /// when building an Inertia response off that path (queue workers,
    /// scheduled tasks, CLI) where no panic net applies, or whenever you
    /// want to handle the serialization failure explicitly.
    pub fn try_data<T>(component: &'static str, dto: T) -> Result<InertiaResponse, FrameworkError>
    where
        T: IntoInertiaData,
    {
        Ok(InertiaResponse::from_data_props(
            component,
            dto.__try_into_inertia_props()?,
        ))
    }

    /// Install the standard Inertia protocol middleware globally.
    ///
    /// Registers two global middlewares in order:
    /// 1. [`InertiaVersionMiddleware`] — emits `409 Conflict` +
    ///    `X-Inertia-Location` when the client's `X-Inertia-Version`
    ///    header doesn't match the server's configured version.
    ///    Without it, asset-version mismatches are silent and stale
    ///    clients keep hitting the new server with the old bundle.
    /// 2. [`Inertia303Middleware`] — converts `302` redirects on
    ///    non-GET Inertia visits to `303`, so the client's follow-up
    ///    request is explicitly a GET. Without it, browsers may
    ///    re-submit the original PUT/PATCH/DELETE to the redirect
    ///    target — silently breaking form-create-then-redirect flows.
    ///
    /// Both middlewares were previously opt-in via the `global_middleware!`
    /// macro. Closes ChatGPT MODULE_REVIEW_NOTES ## inertia MEDIUM #1
    /// (Domain 20 audit D20-F): generated apps that forgot either
    /// middleware quietly got stale-asset behaviour or method-preserving
    /// redirects in production.
    ///
    /// Call once at boot. The `config.version` value is cloned out of
    /// the supplied `InertiaConfig` so callers can keep ownership of
    /// the config for `InertiaResponse::with_config(...)`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use suprnova::{Inertia, InertiaConfig};
    ///
    /// pub fn register() {
    ///     Inertia::install(
    ///         &InertiaConfig::new().version(env!("CARGO_PKG_VERSION")),
    ///     );
    /// }
    /// ```
    pub fn install(config: &InertiaConfig) {
        use crate::middleware::register_global_middleware;
        let version = config.version.clone();
        register_global_middleware(InertiaVersionMiddleware::with_resolver(move || {
            version.resolve()
        }));
        register_global_middleware(Inertia303Middleware::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::get_global_middleware;

    #[test]
    fn install_registers_two_middlewares() {
        let before = get_global_middleware().len();
        Inertia::install(&InertiaConfig::new().version("test-version"));
        let after = get_global_middleware().len();
        assert_eq!(
            after - before,
            2,
            "Inertia::install should register exactly two middlewares \
             (version + 303), got delta={}",
            after - before
        );
    }
}
