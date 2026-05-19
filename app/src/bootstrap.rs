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

use std::sync::Arc;
use std::time::Duration;

#[allow(unused_imports)]
use suprnova::{
    bind, global_middleware, singleton, App, EventFacade, FrameworkError, IncludeMiddleware,
    InertiaRequestExt, InertiaSharedData, InertiaVersionMiddleware, Prop, S3Config, SessionConfig,
    SessionMiddleware, Storage, SupervisorRegistry, UserProvider, DB,
};
use suprnova::broadcasting::{BroadcastHub, ChannelRegistry, InMemoryBroadcastHub};
use suprnova::features::{bootstrap_database_cached, FeatureMiddleware};
use suprnova::queue::worker::register_job;

use crate::broadcasting::{ChatChannel, UserRegisteredChannel};
use crate::events::UserRegistered;
use crate::listeners::SendWelcomeEmailListener;
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

    // Broadcasting hub — in-process pub/sub. Registered in the container
    // as `dyn BroadcastHub` so SSE + WS handlers resolve it uniformly.
    let hub: Arc<dyn BroadcastHub> = Arc::new(InMemoryBroadcastHub::new());
    App::bind::<dyn BroadcastHub>(Arc::clone(&hub));

    // Channel registry — lists every named channel the WS handler accepts.
    // Registered as a concrete singleton so routes::register() can resolve
    // it when constructing BroadcastingWsHandler.
    let mut registry = ChannelRegistry::new();
    registry.register(UserRegisteredChannel);
    registry.register(ChatChannel);
    App::singleton(Arc::new(registry));

    // Event listeners. Registered once at boot; survive for the lifetime
    // of the process. The framework's dispatcher invokes them in
    // registration order.
    EventFacade::listen::<UserRegistered, _>(Arc::new(SendWelcomeEmailListener)).await;

    // Wire UserRegistered → BroadcastHub so every dispatch also publishes
    // the event's JSON payload on the "user_registered" channel. Both SSE
    // and WS subscribers receive it from the same hub.
    EventFacade::broadcast::<UserRegistered>(Arc::clone(&hub)).await;

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

    // Phase 7B T10 — supervised long-running tasks.
    //
    // Spawn every supervisor registered via `inventory::submit!` into its own
    // restart-loop task. The tasks are detached (v1 does not drain them on
    // shutdown); they run for the lifetime of the process.
    SupervisorRegistry::start_all().await;

    // Phase 13 — feature flags.
    //
    // Wire the canonical Cached(Database) chain. After this call:
    //
    // * `is_enabled!("flag-name", default)` resolves through the
    //   per-process cache backed by the `features` table.
    // * `admin::upsert` / `admin::delete` propagate to the live
    //   evaluator before returning (sub-second kill-switch semantics).
    // * `FeatureMiddleware` below opens a per-request context with the
    //   authenticated user_id; no team extraction wired in this app
    //   since we don't yet have a multi-tenant story.
    //
    // 60-second TTL is a sensible default: long enough to amortize the
    // scope-resolution walk for hot paths, short enough that an
    // out-of-band SQL edit (e.g. ops console) reflects within the
    // minute. Operator-initiated changes via admin::* bypass the TTL
    // entirely via the FeatureSync fan-out.
    bootstrap_database_cached(Duration::from_secs(60))
        .await
        .expect("feature-flag chain wired");

    // Global middleware: opens a featureflag::Context per request so
    // user-scoped flags (`is_enabled!("...", default)` inside any
    // handler) see the right scope. Placed after SessionMiddleware so
    // Auth::id() returns the live session's user id.
    global_middleware!(FeatureMiddleware::new());
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
