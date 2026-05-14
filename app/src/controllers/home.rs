use suprnova::{
    App, FrameworkError, HttpResponse, InertiaProps, InertiaResponse, Request, Response,
};

use crate::actions::example_action::ExampleAction;

#[derive(InertiaProps)]
pub struct User {
    pub name: String,
    pub email: String,
}

#[derive(InertiaProps)]
pub struct Stats {
    pub visits: u32,
    pub likes: u32,
}

pub async fn index(req: Request) -> Response {
    let action = App::resolve::<ExampleAction>()?;
    let message = action.execute();

    // Dogfood the full Tier 0–2 builder API. Mixes eager props with
    // Lazy / Defer / Merge / Once / Flash so every variant runs against
    // a real handler. The macro (`inertia_response!`) only handles the
    // typed-eager case — anything more interesting uses the builder.
    let resp = InertiaResponse::new("Home")
        .with("title", "Welcome to Suprnova!")
        .with("message", message)
        .with(
            "user",
            User {
                name: "John Doe".to_string(),
                email: "john@example.com".to_string(),
            },
        )
        .with(
            "stats",
            Stats {
                visits: 1234,
                likes: 567,
            },
        )
        // Lazy: closure only runs when the prop will actually be sent.
        .lazy("recent_activity", || async {
            Ok::<_, FrameworkError>(suprnova::serde_json::json!([
                {"action": "signed in", "at": "just now"},
                {"action": "viewed dashboard", "at": "2 min ago"},
            ]))
        })
        // Defer: not resolved on initial visit; client fetches via a
        // follow-up partial reload. Emitted under `deferredProps`.
        .defer("notifications", || async {
            Ok::<_, FrameworkError>(suprnova::serde_json::json!([
                {"id": 1, "msg": "Welcome aboard"},
            ]))
        })
        // Merge: appends to the array on partial reloads instead of
        // replacing. Useful for "load more" pagination.
        .merge("tags", suprnova::serde_json::json!(["rust", "framework"]))
        // Once: cached on the client across navigations; resolver
        // skipped on subsequent visits via X-Inertia-Except-Once-Props.
        .once("plans", || async {
            Ok::<_, FrameworkError>(suprnova::serde_json::json!([
                {"id": 1, "name": "Free"},
                {"id": 2, "name": "Pro"},
            ]))
        })
        // Flash: one-shot toast appearing under page.flash (not in props).
        .flash(
            "toast",
            suprnova::serde_json::json!({"type": "info", "msg": "Welcome!"}),
        )
        .resolve(&req)
        .await
        .map_err(HttpResponse::from)?;

    Ok(resp)
}
