mod group;
mod macros;
mod router;

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
pub use router::{
    BoxedHandler, MultiMethodRouteBuilder, RouteBuilder, Router, WsMatch,
    clear_route_names_for_test, register_route_name, route, route_with_params,
    try_register_route_name,
};
