use suprnova::{get, group, post, routes};

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

    // Protected routes - requires Authorization header
    group!("/protected", {
        get!("/", controllers::home::index).name("protected.home"),
    }).middleware(AuthMiddleware),

    // Todo routes group
    group!("/todos", {
        get!("/", controllers::todo::list).name("todos.index"),
        post!("/random", controllers::todo::create_random).name("todos.create_random"),
    }),
}
