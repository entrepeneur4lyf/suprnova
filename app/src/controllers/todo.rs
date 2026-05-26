use suprnova::{App, Request, Response, ResponseExt, json_response};

use crate::actions::todo_action::{CreateRandomTodoAction, ListTodosAction};

pub async fn create_random(_req: Request) -> Response {
    let action = App::resolve::<CreateRandomTodoAction>()?;

    match action.execute().await {
        Ok(todo) => json_response!({
            "success": true,
            "todo": todo
        })
        .status(201),
        Err(e) => json_response!({
            "success": false,
            "error": e.to_string()
        })
        .status(500),
    }
}

pub async fn list(_req: Request) -> Response {
    let action = App::resolve::<ListTodosAction>()?;

    match action.execute().await {
        Ok(todos) => json_response!({
            "success": true,
            "todos": todos
        })
        .status(200),
        Err(e) => json_response!({
            "success": false,
            "error": e.to_string()
        })
        .status(500),
    }
}
