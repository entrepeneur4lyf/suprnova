use suprnova::{get, post, routes};

use crate::controllers;

routes! {
    // Users
    get!("/api/users",     controllers::users::list_users),
    get!("/api/users/:id", controllers::users::show_user),

    // Authentication (public)
    post!("/api/auth/register", controllers::users::register),
    post!("/api/auth/login",    controllers::users::login),
}
