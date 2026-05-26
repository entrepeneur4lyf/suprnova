mod config;
mod conversion_middleware;
mod encrypt_middleware;
mod facade;
pub(crate) mod flash;
mod manifest;
mod prop;
mod response;
mod shared;
pub(crate) mod ssr;
mod version_middleware;

pub use config::{Frontend, InertiaConfig, SsrConfig, VersionResolver};
pub use conversion_middleware::Inertia303Middleware;
pub use encrypt_middleware::EncryptHistoryMiddleware;
pub use facade::Inertia;
pub use manifest::{ManifestEntry, ResolvedAssets, ViteManifest};
pub use prop::{
    DeferConfig, DeferOptions, InertiaRequestExt, MergeConfig, MergeStrategy, OnceConfig,
    OnceOptions, PartialFilter, Prop, PropFuture, PropResolver, ScrollConfig, ScrollMetadata,
};
pub use response::{InertiaResponse, IntoInertiaData, PropEntry};
pub use shared::{InertiaRegistry, InertiaSharedData};
pub use ssr::SsrResponse;
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
