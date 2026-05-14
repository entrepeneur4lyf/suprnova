mod config;
pub(crate) mod flash;
mod prop;
mod response;
mod shared;
mod version_middleware;

pub use config::{Frontend, InertiaConfig};
pub use prop::{
    DeferConfig, DeferOptions, InertiaRequestExt, MergeConfig, MergeStrategy, OnceConfig,
    OnceOptions, PartialFilter, Prop, PropFuture, PropResolver,
};
pub use response::InertiaResponse;
pub use shared::{InertiaRegistry, InertiaSharedData};
pub use version_middleware::InertiaVersionMiddleware;

// Test helpers for setting up a flash scope outside of a real server.
// Production code never calls these — the flash scope is set up
// automatically by `Server::handle_request`.
#[doc(hidden)]
pub fn flash_new_bag_for_test()
-> std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>> {
    flash::new_bag()
}

#[doc(hidden)]
pub async fn flash_scope_for_test<F: std::future::Future>(
    bag: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>>,
    fut: F,
) -> F::Output {
    flash::FLASH_BAG.scope(bag, fut).await
}
