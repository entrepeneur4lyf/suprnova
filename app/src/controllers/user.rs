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
