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
    bind, global_middleware, singleton, App, EventFacade, FrameworkError, IncludeMiddleware,
    InertiaRequestExt, InertiaSharedData, InertiaVersionMiddleware, Prop, S3Config, SessionConfig,
    SessionMiddleware, Storage, UserProvider, DB,
};
use suprnova::queue::worker::register_job;
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

    // Phase 3: scope `?include=` and `?fields[type]=` from the query string
    // into task-local state so JSON:API resource handlers can read them.
    global_middleware!(IncludeMiddleware);

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
        .set(user_registered_tx.clone()).expect("bootstrap::register() called twice; USER_REGISTERED_SENDER already set");

    EventFacade::listen::<UserRegistered, _>(Arc::new(SendWelcomeEmailListener)).await;
    EventFacade::listen::<UserRegistered, _>(Arc::new(UserRegisteredBroadcaster::new(
        user_registered_tx,
    )))
    .await;

    // Storage disks. Local `public` is always available; the S3 `uploads`
    // disk is env-gated so dev boots without AWS credentials.
    register_storage_disks();

    // Session middleware — installed globally so every route shares the
    // same session lifecycle. The framework's `AuthMiddleware` and the
    // `Auth::user_as` facade both read state set up by this middleware,
    // so wiring it here is what makes auth-aware controllers (e.g. the
    // avatar upload endpoint) functional in dev/prod.
    global_middleware!(SessionMiddleware::new(SessionConfig::from_env()));

    // Bootstrap rate-limit driver so App::resolve_make::<dyn RateLimiter>()
    // succeeds when routes::register() runs immediately after bootstrap.
    // Server::run also calls bootstrap_from_env() later — that call is
    // idempotent (guarded by has_binding) so there is no double-init.
    suprnova::rate_limit::bootstrap_default().await;

    // Register queue jobs so the worker can dispatch them by name.
    register_job::<crate::jobs::welcome_log::WelcomeLog>();

    // Phase 5B Task 20 — mail dogfood.
    //
    // Register the WelcomeEmail mailable factory so the worker can
    // re-hydrate it from a `SendMailJob` envelope, and register
    // `SendMailJob` itself as a worker-dispatchable job. Both calls are
    // idempotent (last-write-wins) per the framework's registry contract.
    suprnova::mail::register_mailable_factory::<crate::mail::welcome::WelcomeEmail>();
    register_job::<suprnova::mail::send_job::SendMailJob>();

    // Phase 5B Task 20 — notifications dogfood.
    //
    // Register an OrderShipped factory so `SendNotificationJob` can rebuild
    // the notification from its JSON payload at dispatch time, and register
    // the notification job for worker dispatch. The factory is now
    // auto-derived from `N: Notification`'s `Deserialize` impl.
    suprnova::notifications::register_notification_factory::<
        crate::notifications::order_shipped::OrderShipped,
    >();
    register_job::<suprnova::notifications::notify_job::SendNotificationJob>();

    // Phase 6A T7 — factory + seeder dogfood. Registers `BaseSeeder`
    // so a `suprnova db:seed` invocation (Phase 6B) will populate 50
    // users + 200 posts via the framework's Factory / Persistable
    // path. Tests reach the same path through `seed::run_all()`
    // directly without the CLI.
    suprnova::seed::register::<crate::seeders::BaseSeeder>();
}

/// Register the application's storage disks.
///
/// `public` is a local-filesystem disk rooted at `./storage/public`, suitable
/// for development. `uploads` is an S3-backed disk that is only registered
/// when `S3_BUCKET` is set in the environment — production deployments wire
/// it via env vars, while local dev and tests skip it.
///
/// Split out of `register()` so test harnesses can re-target the `public`
/// disk to a tempdir without re-running the rest of bootstrap.
pub fn register_storage_disks() {
    Storage::register_fs("public", "./storage/public").expect("register public disk");

    // Pre-create the `avatars/` subdirectory so the first upload to a
    // freshly-cloned checkout doesn't 500 on a missing parent. opendal's
    // fs service creates files but not intermediate dirs.
    std::fs::create_dir_all("./storage/public/avatars").ok();

    if let Ok(bucket) = std::env::var("S3_BUCKET") {
        Storage::register_s3(
            "uploads",
            S3Config {
                bucket,
                region: std::env::var("AWS_REGION").ok(),
                endpoint: std::env::var("S3_ENDPOINT").ok(),
                access_key_id: std::env::var("AWS_ACCESS_KEY_ID").ok(),
                secret_access_key: std::env::var("AWS_SECRET_ACCESS_KEY").ok(),
                root: std::env::var("S3_ROOT").ok(),
            },
        )
        .expect("register S3 uploads disk");
    }
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
