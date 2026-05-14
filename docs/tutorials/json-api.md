---
title: 'Build a Todo JSON API'
description: 'Create a complete CRUD REST API for todos'
icon: 'code'
---

In this tutorial, you'll build a complete JSON API for managing todos. You'll learn how to create routes, controllers, and interact with the database.

## What We're Building

A REST API with these endpoints:

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/todos` | List all todos |
| GET | `/api/todos/{todo}` | Get a single todo |
| POST | `/api/todos` | Create a todo |
| PUT | `/api/todos/{todo}` | Update a todo |
| DELETE | `/api/todos/{todo}` | Delete a todo |

## Prerequisites

Make sure you have a suprnova project created:

```bash
suprnova new todo-api
cd todo-api
```

## Step 1: Create the Migration

First, create a migration for the todos table:

```bash
suprnova make:migration create_todos_table
```

Edit the generated migration file in `migrations/`:

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Todos::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Todos::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Todos::Title).string().not_null())
                    .col(ColumnDef::new(Todos::Description).text().null())
                    .col(
                        ColumnDef::new(Todos::Completed)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Todos::CreatedAt).timestamp().not_null())
                    .col(ColumnDef::new(Todos::UpdatedAt).timestamp().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Todos::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Todos {
    Table,
    Id,
    Title,
    Description,
    Completed,
    CreatedAt,
    UpdatedAt,
}
```

Run the migration and sync entities:

```bash
suprnova migrate
suprnova db:sync
```

## Step 2: Create the Controller

Create a controller for handling todo operations:

```bash
suprnova make:controller todos
```

Edit `src/controllers/todos.rs`:

```rust
use suprnova::{handler, request, json_response, Request, Response};
use suprnova::database::{Model, ModelMut};
use crate::models::todos::{Entity as Todos, ActiveModel, Model as Todo};
use sea_orm::ActiveValue::Set;

// Request struct for creating todos with validation
#[request]
pub struct CreateTodoRequest {
    #[validate(length(min = 1, max = 255, message = "Title is required"))]
    pub title: String,
    #[validate(length(max = 1000))]
    pub description: Option<String>,
}

// Request struct for updating todos with validation
#[request]
pub struct UpdateTodoRequest {
    #[validate(length(min = 1, max = 255))]
    pub title: Option<String>,
    #[validate(length(max = 1000))]
    pub description: Option<String>,
    pub completed: Option<bool>,
}

// GET /api/todos
#[handler]
pub async fn index(_req: Request) -> Response {
    match Todos::all().await {
        Ok(todos) => json_response!({
            "data": todos,
            "count": todos.len()
        }),
        Err(e) => json_response!({
            "error": e.to_string()
        }, 500),
    }
}

// GET /api/todos/{todo} - Route model binding automatically fetches the todo
#[handler]
pub async fn show(todo: Todo) -> Response {
    json_response!({"data": todo})
}

// POST /api/todos
#[handler]
pub async fn store(form: CreateTodoRequest) -> Response {
    // `form` is already validated - returns 422 with errors if invalid
    let now = chrono::Utc::now().naive_utc();

    let new_todo = ActiveModel {
        title: Set(form.title),
        description: Set(form.description),
        completed: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    match Todos::insert_one(new_todo).await {
        Ok(result) => json_response!({
            "message": "Todo created",
            "id": result.last_insert_id
        }, 201),
        Err(e) => json_response!({"error": e.to_string()}, 500),
    }
}

// PUT /api/todos/{todo} - Route model binding automatically fetches the todo
#[handler]
pub async fn update(todo: Todo, form: UpdateTodoRequest) -> Response {
    let mut active: ActiveModel = todo.into();

    if let Some(title) = form.title {
        active.title = Set(title);
    }
    if let Some(description) = form.description {
        active.description = Set(Some(description));
    }
    if let Some(completed) = form.completed {
        active.completed = Set(completed);
    }
    active.updated_at = Set(chrono::Utc::now().naive_utc());

    match Todos::update_one(active).await {
        Ok(updated) => json_response!({"data": updated}),
        Err(e) => json_response!({"error": e.to_string()}, 500),
    }
}

// DELETE /api/todos/{todo} - Route model binding automatically fetches the todo
#[handler]
pub async fn destroy(todo: Todo) -> Response {
    match Todos::delete_by_pk(todo.id).await {
        Ok(_) => json_response!({"message": "Todo deleted"}),
        Err(e) => json_response!({"error": e.to_string()}, 500),
    }
}
```

## Step 3: Define Routes

Add the routes in `src/routes.rs`:

```rust
use suprnova::{routes, get, post, put, delete};
use crate::controllers::todos;

routes! {
    // API Routes
    get!("/api/todos", todos::index),
    get!("/api/todos/{todo}", todos::show),
    post!("/api/todos", todos::store),
    put!("/api/todos/{todo}", todos::update),
    delete!("/api/todos/{todo}", todos::destroy),
}
```

The `routes!` macro automatically generates a `register()` function that returns a configured `Router`.

## Step 4: Test the API

Start the server:

```bash
suprnova serve --backend-only
```

### Create a Todo

```bash
curl -X POST http://localhost:8080/api/todos \
  -H "Content-Type: application/json" \
  -d '{"title": "Learn suprnova", "description": "Build awesome Rust apps"}'
```

Response:
```json
{
  "message": "Todo created",
  "id": 1
}
```

### List All Todos

```bash
curl http://localhost:8080/api/todos
```

Response:
```json
{
  "data": [
    {
      "id": 1,
      "title": "Learn suprnova",
      "description": "Build awesome Rust apps",
      "completed": false,
      "created_at": "2024-01-15T12:00:00",
      "updated_at": "2024-01-15T12:00:00"
    }
  ],
  "count": 1
}
```

### Get a Single Todo

```bash
# {todo} is the todo ID - suprnova automatically fetches the model
curl http://localhost:8080/api/todos/1
```

### Update a Todo

```bash
# {todo} is the todo ID - suprnova automatically fetches the model
curl -X PUT http://localhost:8080/api/todos/1 \
  -H "Content-Type: application/json" \
  -d '{"completed": true}'
```

### Delete a Todo

```bash
# {todo} is the todo ID - suprnova automatically fetches the model
curl -X DELETE http://localhost:8080/api/todos/1
```

## Adding Validation

The `#[request]` attribute automatically handles validation using the `validator` crate. When validation fails, suprnova returns a 422 response with Laravel/Inertia-compatible error format:

```rust
use suprnova::{handler, request, json_response, Response};

#[request]
pub struct CreateTodoRequest {
    #[validate(length(min = 1, max = 255, message = "Title is required"))]
    pub title: String,
    #[validate(length(max = 1000))]
    pub description: Option<String>,
}

#[handler]
pub async fn store(form: CreateTodoRequest) -> Response {
    // `form` is already validated - this code only runs if validation passes
    // Returns 422 with error details if validation fails

    // ... rest of the handler
    json_response!({"message": "Todo created"}, 201)
}
```

If validation fails, the response looks like:

```json
{
    "message": "The given data was invalid.",
    "errors": {
        "title": ["Title is required"]
    }
}
```

## Summary

You've built a complete CRUD API with:

- Database migrations for the todos table
- A controller with index, show, store, update, and destroy actions
- RESTful routes following conventions
- JSON responses with proper error handling

## Next Steps

- Add authentication middleware to protect routes
- Implement pagination for the index endpoint
- Add filtering and sorting capabilities
- Create an Inertia frontend (see [Inertia Todo Tutorial](/tutorials/inertia-crud))
