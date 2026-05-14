use suprnova::{json_response, redirect, route, Request, Response, ResponseExt};

pub async fn index(_req: Request) -> Response {
    json_response!({
        "users": [
            {"id": 1, "name": "John"},
            {"id": 2, "name": "Jane"}
        ]
    })
    .status(200)
}

pub async fn show(req: Request) -> Response {
    let id = req.param("id")?;
    json_response!({
        "id": id,
        "name": format!("User {}", id)
    })
}

/// Example: Create a user and redirect to the user list
pub async fn store(_req: Request) -> Response {
    // ... create user logic would go here ...

    // Redirect to users.index (compile-time validated!)
    redirect!("users.index").into()
}

/// Example: Redirect to a specific user with query params
pub async fn redirect_example(_req: Request) -> Response {
    // Generate a URL using route()
    let url = route("users.show", &[("id", "42")]);
    println!("Generated URL: {:?}", url);

    // Redirect with query parameters (compile-time validated!)
    redirect!("users.index")
        .query("page", "1")
        .query("sort", "name")
        .into()
}

/// Example: Inertia redirect that preserves the URL fragment across
/// the redirect. The destination's `InertiaResponse` will emit
/// `preserveFragment: true` so the client carries over its current
/// `#anchor` to the new URL.
///
/// Maps to Laravel's `redirect()->preserveFragment()`.
pub async fn preserve_fragment_example(_req: Request) -> Response {
    redirect!("users.index").preserve_fragment().into()
}

/// Example: opt out of SSR for this specific request. The destination
/// renders client-side only even when `InertiaConfig::ssr.enabled` is
/// `true`. Useful for routes that depend on per-request state SSR
/// can't see (geolocation, session-only flash, etc.) or for debugging.
///
/// Maps to Laravel's `Inertia::disable_ssr()`.
pub async fn ssr_opt_out_example(_req: Request) -> Response {
    suprnova::App::disable_ssr_for_request();
    json_response!({
        "ssr_disabled_for_this_request": true,
        "note": "If SSR is enabled globally, this route still renders CSR-only.",
    })
}
