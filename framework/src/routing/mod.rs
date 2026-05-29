mod group;
mod macros;
mod resource;
mod router;
mod signed;
pub mod url;

pub use group::{GroupBuilder, GroupRouter};
pub use macros::{
    // Internal functions used by macros (hidden from docs)
    __any_impl,
    __delete_impl,
    __fallback_impl,
    __get_impl,
    __head_impl,
    __options_impl,
    __patch_impl,
    __post_impl,
    __put_impl,
    __ws_impl,
    AnyRouteDefBuilder,
    FallbackDefBuilder,
    GroupAnyRoute,
    GroupDef,
    GroupItem,
    GroupRoute,
    HttpMethod,
    IntoGroupItem,
    RouteDefBuilder,
    WsRouteDef,
    validate_route_path,
};
pub use resource::{ResourceAction, ResourceController, ResourceRoutes};
pub use router::{
    BoxedHandler, MultiMethodRouteBuilder, RouteBuilder, Router, WsMatch,
    clear_route_names_for_test, register_route_name, route, route_name_for_pattern,
    route_with_params, try_register_route_name,
};
pub use signed::{
    EXPIRES_KEY, SIGNATURE_KEY, SignatureVerdict, sign_route, sign_url, verify_signature,
};

/// Top-level `redirect()` helper. Laravel's `redirect()` global with no
/// arguments returns a `Redirector` you chain methods on; Rust's
/// argument-less call here returns a [`crate::http::Redirect::to`]
/// to `/` so the common case (`return redirect()`) compiles without a
/// path argument. Pass a path to redirect there:
///
/// ```rust,ignore
/// use suprnova::redirect;
///
/// // bare → /
/// let r = redirect();
///
/// // explicit → /dashboard
/// let r = redirect_to("/dashboard");
/// ```
///
/// For named-route redirects use [`crate::Redirect::route`]; for
/// session-aware previous-URL redirects use [`crate::Redirect::back`].
pub fn redirect() -> crate::http::Redirect {
    crate::http::Redirect::to("/")
}

/// `redirect_to(path)` — Rust-side shorthand for
/// [`crate::http::Redirect::to`]. Identical behaviour; provided so call
/// sites can write `redirect_to("/dashboard")` instead of the longer
/// `Redirect::to("/dashboard")`.
pub fn redirect_to(path: impl Into<String>) -> crate::http::Redirect {
    crate::http::Redirect::to(path)
}
