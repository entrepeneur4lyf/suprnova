mod group;
mod macros;
mod router;

pub use group::{GroupBuilder, GroupRouter};
pub use macros::{
    // Internal functions used by macros (hidden from docs)
    __delete_impl, __fallback_impl, __get_impl, __post_impl, __put_impl, validate_route_path,
    FallbackDefBuilder, GroupDef, GroupItem, GroupRoute, HttpMethod, IntoGroupItem,
    RouteDefBuilder,
};
pub use router::{
    register_route_name, route, route_with_params, BoxedHandler, RouteBuilder, Router, WsMatch,
};
