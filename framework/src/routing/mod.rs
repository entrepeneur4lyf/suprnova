mod group;
mod macros;
mod router;

pub use group::{GroupBuilder, GroupRouter};
pub use macros::{
    // Internal functions used by macros (hidden from docs)
    __delete_impl,
    __fallback_impl,
    __get_impl,
    __post_impl,
    __put_impl,
    __ws_impl,
    FallbackDefBuilder,
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
    BoxedHandler, RouteBuilder, Router, WsMatch, register_route_name, route, route_with_params,
    try_register_route_name,
};
