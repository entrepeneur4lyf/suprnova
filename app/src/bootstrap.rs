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

use std::sync::{Arc, OnceLock};

#[allow(unused_imports)]
use suprnova::{
    bind, global_middleware, singleton, App, EventFacade, FrameworkError, InertiaRequestExt,
    InertiaSharedData, InertiaVersionMiddleware, Prop, UserProvider, DB,
};
use tokio::sync::broadcast;

use crate::events::UserRegistered;
use crate::listeners::{SendWelcomeEmailListener, UserRegisteredBroadcaster};
use crate::middleware;
use crate::providers::DatabaseUserProvider;

/// Capacity of the `UserRegistered` broadcast channel. Tokio's
/// broadcast channel is bounded; if subscribers fall behind by more
/// than this many messages, they get a `Lagged(n)` error on `recv`
/// and the sender keeps moving. 64 gives small bursts headroom on
/// the SSE feed without growing memory if a tab hangs.
const USER_REGISTERED_CHANNEL_CAPACITY: usize = 64;

/// Process-global sender for the `UserRegistered` broadcast feed.
/// Initialized in `register()`; read by `controllers::sse_example`.
/// `OnceLock` (not `Mutex<Option<_>>`) because we set it once at
/// boot and never need to mutate it again.
static USER_REGISTERED_SENDER: OnceLock<Arc<broadcast::Sender<UserRegistered>>> = OnceLock::new();

/// Accessor for the broadcast sender. Panics if called before
/// `bootstrap::register()` has run — that path indicates a wiring
/// bug, not a runtime condition.
pub fn user_registered_sender() -> Arc<broadcast::Sender<UserRegistered>> {
    USER_REGISTERED_SENDER
        .get()
        .expect("user_registered_sender called before bootstrap::register()")
        .clone()
}

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

    // Event listeners. Registered once at boot; survive for the lifetime
    // of the process. The framework's dispatcher invokes them in
    // registration order, so the broadcaster fires after the welcome-email
    // logger — keep it that way if you care about SSE seeing only events
    // the other listeners have processed.
    let (user_registered_tx, _) = broadcast::channel(USER_REGISTERED_CHANNEL_CAPACITY);
    let user_registered_tx = Arc::new(user_registered_tx);
    // Park the sender in the OnceLock so the SSE handler can subscribe.
    // `set` returns Err if already populated — register() should only run
    // once per process; treat a double-init as a bug.
    USER_REGISTERED_SENDER
        .set(user_registered_tx.clone())
        .ok()
        .expect("bootstrap::register() called twice; USER_REGISTERED_SENDER already set");

    EventFacade::listen::<UserRegistered, _>(Arc::new(SendWelcomeEmailListener)).await;
    EventFacade::listen::<UserRegistered, _>(Arc::new(UserRegisteredBroadcaster::new(
        user_registered_tx,
    )))
    .await;
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
