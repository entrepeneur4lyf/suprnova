//! Application Bootstrap
//!
//! This is where you register global middleware and services that need runtime configuration.
//! Services that don't need runtime config can use `#[service(ConcreteType)]` instead.
//!
//! # Example
//!
//! ```rust,ignore
//! // For services with no runtime config, use the macro:
//! #[service(RedisCache)]
//! pub trait CacheStore { ... }
//!
//! // For services needing runtime config, register here:
//! pub async fn register() {
//!     // Initialize database
//!     DB::init().await.expect("Failed to connect to database");
//!
//!     // Global middleware
//!     global_middleware!(middleware::LoggingMiddleware);
//!
//!     // Services
//!     bind!(dyn Database, PostgresDB::new());
//! }
//! ```

#[allow(unused_imports)]
use suprnova::{
    bind, global_middleware, singleton, App, FrameworkError, InertiaRequestExt,
    InertiaSharedData, InertiaVersionMiddleware, Prop, UserProvider, DB,
};

use crate::middleware;
use crate::providers::DatabaseUserProvider;

/// Register global middleware and services
///
/// Called from cmd/main.rs before `Server::from_config()`.
/// Middleware and services registered here can use environment variables, config files, etc.
pub async fn register() {
    // Initialize database connection
    DB::init().await.expect("Failed to connect to database");

    // Global middleware (runs on every request in registration order)
    global_middleware!(middleware::LoggingMiddleware);

    // Asset-version 409 middleware — sends clients with stale SPA bundles
    // through a full-page reload when the server's version has bumped.
    // Without this, asset-version mismatches are silent. Version string
    // here matches `InertiaConfig::default().version` ("1.0" today);
    // when we wire `cargo build` to stamp a real hash we'll pass it
    // through env or a build-script-generated const.
    global_middleware!(InertiaVersionMiddleware::new("1.0"));

    // Register the user provider for Auth::user()
    bind!(dyn UserProvider, DatabaseUserProvider);

    // Inertia shared data — visible on every Inertia response.
    //
    // Static values: process-global, set once at boot.
    App::inertia_share("appName", "Suprnova");
    App::inertia_share("appVersion", env!("CARGO_PKG_VERSION"));

    // Per-request shared data via the trait. The framework awaits
    // `share(&req)` on every Inertia response so this can read headers,
    // session, etc. — see `AppSharedData` below.
    App::register_inertia_shared(std::sync::Arc::new(AppSharedData));
}

/// Per-request Inertia shared data. Demonstrates the request-aware
/// pattern for things like the authenticated user, locale, or flash.
///
/// In a real app this would call `Auth::user()` to surface the current
/// user; for the dogfood test bed we synthesize a placeholder so the
/// frontend can render the layout without depending on the auth flow.
struct AppSharedData;

#[suprnova::async_trait]
impl InertiaSharedData for AppSharedData {
    async fn share(
        &self,
        _req: &dyn InertiaRequestExt,
    ) -> Result<suprnova::indexmap::IndexMap<String, Prop>, FrameworkError> {
        let mut shared = suprnova::indexmap::IndexMap::new();
        shared.insert(
            "auth".to_string(),
            Prop::Eager(suprnova::serde_json::json!({
                "user": {
                    "name": "Demo User",
                    "email": "demo@suprnova.app",
                }
            })),
        );
        Ok(shared)
    }
}
