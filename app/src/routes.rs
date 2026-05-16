use suprnova::{delete, get, group, post, routes, AuthMiddleware as SessionAuthMiddleware};

use crate::controllers;
use crate::middleware::AuthMiddleware;

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
    // Gate::authorize through PostPolicy for show). The public listing
    // lives at `/api/posts/public` so it doesn't share its
    // route_middleware key with `POST /api/posts`, which is auth-gated.
    // The framework's middleware map is keyed only on path (not method),
    // so a public + auth route on the same path string would inherit
    // the auth middleware on both.
    get!("/api/posts/public", controllers::posts::index).name("api.posts.public"),
    group!("/api/posts", {
        get!("/{id}", controllers::posts::show).name("api.posts.show"),
        post!("/", controllers::posts::store).name("api.posts.store"),
    }).middleware(SessionAuthMiddleware::new()),
}
