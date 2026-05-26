use std::sync::Arc;
use std::time::Duration;
use suprnova::broadcasting::{
    BroadcastHub, BroadcastingWsHandler, ChannelRegistry, InMemoryBroadcastHub,
};
use suprnova::{
    AuthMiddleware as SessionAuthMiddleware, RateLimitMiddleware, RateLimiter, SlidingWindowConfig,
    container::App, delete, get, group, post, rate_limit::memory::InMemoryRateLimiter, routes, ws,
};

use crate::broadcasting::{ChatChannel, UserRegisteredChannel};
use crate::controllers;
use crate::middleware::AuthMiddleware;
use crate::ws as app_ws;

/// Build the `BroadcastingWsHandler` for `/ws/broadcast` by resolving
/// the hub and channel registry from the App container.
///
/// Falls back to a fresh in-process hub + registry when the container
/// hasn't been bootstrapped (e.g. in unit tests that assemble the
/// router without running `bootstrap::register()`). This mirrors the
/// pattern used by the rate-limit middleware.
fn broadcasting_handler() -> BroadcastingWsHandler {
    let hub: Arc<dyn BroadcastHub> =
        App::make::<dyn BroadcastHub>().unwrap_or_else(|| Arc::new(InMemoryBroadcastHub::new()));
    let registry: Arc<ChannelRegistry> = App::get::<Arc<ChannelRegistry>>().unwrap_or_else(|| {
        let mut r = ChannelRegistry::new();
        r.register(UserRegisteredChannel);
        r.register(ChatChannel);
        Arc::new(r)
    });
    BroadcastingWsHandler::new(hub, registry)
}

routes! {
    get!("/", controllers::home::index).name("home"),
    get!("/redirect-example", controllers::user::redirect_example),
    get!(
        "/preserve-fragment-example",
        controllers::user::preserve_fragment_example
    ),
    get!(
        "/ssr-opt-out-example",
        controllers::user::ssr_opt_out_example
    ),
    get!("/config", controllers::config_example::show).name("config.show"),

    // User routes group
    group!("/users", {
        get!("/", controllers::user::index).name("users.index"),
        get!("/{id}", controllers::user::show).name("users.show"),
        post!("/", controllers::user::store).name("users.store"),
    }),

    // Authenticated user routes — session-gated via the framework's
    // `AuthMiddleware`. The avatar upload exercises the full multipart
    // + storage + Auth stack end-to-end.
    group!("/users", {
        post!("/avatar", controllers::avatar_upload::upload).name("users.avatar.store"),
    }).middleware(SessionAuthMiddleware::new()),

    // Protected routes - requires Authorization header
    group!("/protected", {
        get!("/", controllers::home::index).name("protected.home"),
    }).middleware(AuthMiddleware),

    // Todo routes group
    group!("/todos", {
        get!("/", controllers::todo::list).name("todos.index"),
        post!("/random", controllers::todo::create_random).name("todos.create_random"),
    }),

    // SSE dogfood — streams UserRegistered broadcast events
    get!("/events/stream", controllers::sse_example::stream).name("events.stream"),

    // Phase 7A WebSocket dogfood — echo handler at /ws/echo.
    // Round-trips text messages with an "echo: " prefix; exits on peer close.
    ws!("/ws/echo", app_ws::echo::EchoHandler),

    // Phase 7B WebSocket broadcasting — JSON-envelope subscribe/publish.
    // Clients send {"type":"subscribe","channel":"user_registered"} to
    // receive UserRegistered events; ChatChannel requires a token in data.
    ws!("/ws/broadcast", broadcasting_handler()),

    // Phase 2 dogfood — cursor pagination over a 100-user fixture
    get!("/api/users", controllers::paginated_users::index).name("api.users.index"),

    // Phase 3 dogfood — JSON:API resources + Gate-authorized deletion
    // GET  /api/users/{id}  → JSON:API single resource (sparse fieldsets via ?fields[users]=...)
    // GET  /api/v3/users    → JSON:API collection
    // DELETE /api/posts/{id} → Gate::authorize("delete-post", ...) demo
    get!("/api/users/{id}", controllers::admin::show_user).name("api.users.show"),
    get!("/api/v3/users", controllers::admin::list_users).name("api.v3.users.index"),
    delete!("/api/posts/{id}", controllers::admin::delete_post).name("api.posts.destroy"),

    // Codex finding #17 — real Post model. Public GET listing remains
    // open; create/show require a session (the controllers also enforce
    // Gate::authorize through PostPolicy for show). The framework's
    // middleware map is keyed by `(method, path)` so the public GET
    // and the auth-gated POST can share the `/api/posts` path string
    // without leaking middleware across methods.
    get!("/api/posts", controllers::posts::index).name("api.posts.index"),
    group!("/api/posts", {
        get!("/{id}", controllers::posts::show).name("api.posts.show"),
        post!("/", controllers::posts::store).name("api.posts.store"),
    }).middleware(SessionAuthMiddleware::new()),

    // Phase 5B Task 20 — mail dogfood. `POST /api/welcome?email=...&name=...`
    // queues a WelcomeEmail Mailable onto the mail queue via Mail::queue.
    // The Mailable + SendMailJob are registered in bootstrap::register so
    // the worker can re-hydrate and dispatch.
    post!("/api/welcome", controllers::welcome::queue).name("api.welcome"),

    // Phase 11 — auth-flows dogfood.
    //
    // Public endpoints (no session middleware — they consume tokens
    // minted out-of-band or implement anti-enumeration responses for
    // arbitrary input):
    //   POST /auth/verify/resend?email=...  → 200, anti-enumeration
    //   GET  /auth/verify?token=...         → 302 / on success
    //   POST /auth/password/request         → 200, anti-enumeration
    //   POST /auth/password/reset           → 302 /?reset=ok on success
    //
    // Session-gated endpoints (require an authenticated session via
    // `SessionAuthMiddleware`):
    //   POST /auth/2fa/enroll   → 200 JSON {otpauth_url, qr_code_svg, recovery_codes}
    //   POST /auth/2fa/confirm  → 200 JSON {status:"confirmed"}
    //   POST /auth/2fa/disable  → 200 JSON {status:"disabled"}
    post!("/auth/verify/resend", controllers::auth_verify::resend).name("auth.verify.resend"),
    get!("/auth/verify", controllers::auth_verify::verify).name("auth.verify.confirm"),
    post!("/auth/password/request", controllers::auth_reset::request_reset)
        .name("auth.password.request"),
    post!("/auth/password/reset", controllers::auth_reset::complete_reset)
        .name("auth.password.complete"),
    group!("/auth/2fa", {
        post!("/enroll", controllers::auth_2fa::enroll).name("auth.2fa.enroll"),
        post!("/confirm", controllers::auth_2fa::confirm).name("auth.2fa.confirm"),
        post!("/disable", controllers::auth_2fa::disable).name("auth.2fa.disable"),
    }).middleware(SessionAuthMiddleware::new()),

    // Phase 5A dogfood — rate-limited ping endpoint.
    // 5 requests per 60-second window, keyed by X-Forwarded-For header
    // (falls back to "anon"). The in-memory limiter is bootstrapped in
    // bootstrap::register() so it is available here at route-build time.
    group!("/api", {
        post!("/ping", controllers::ping::pong).name("api.ping"),
    }).middleware({
        // Use the container binding if bootstrap has already wired it
        // (production path); fall back to a fresh in-memory limiter so
        // tests that assemble the router by hand without running
        // bootstrap::register() keep working.
        let limiter: Arc<dyn RateLimiter> = App::resolve_make::<dyn RateLimiter>()
            .unwrap_or_else(|_| Arc::new(InMemoryRateLimiter::new()));
        RateLimitMiddleware::new(
            limiter,
            SlidingWindowConfig {
                max_requests: 5,
                window: Duration::from_secs(60),
            },
            |req| {
                req.header("x-forwarded-for")
                    .map(|v| {
                        format!(
                            "ip:{}",
                            v.split(',').next().unwrap_or("anon").trim()
                        )
                    })
                    .unwrap_or_else(|| "ip:anon".into())
            },
        )
    }),
}
