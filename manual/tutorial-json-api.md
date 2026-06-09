# Build a Todo JSON:API

A walk-through of the API path end-to-end: migration, model, validated
form requests, route model binding, JSON:API resource envelopes,
sparse fieldsets, pagination. By the end you have a five-endpoint
todo service that emits spec-conformant
[JSON:API](https://jsonapi.org/) responses with `?include=` and
`?fields[todos]=...` honoured automatically.

What you'll build:

| Method   | Route                | Action  |
|----------|----------------------|---------|
| `GET`    | `/api/todos`         | list (paginated) |
| `GET`    | `/api/todos/{todo}`  | show |
| `POST`   | `/api/todos`         | create |
| `PUT`    | `/api/todos/{todo}`  | update |
| `DELETE` | `/api/todos/{todo}`  | delete |

## Prerequisites

A scaffolded project:

```bash
suprnova new todo-api
cd todo-api
```

## Step 1: The migration

```bash
suprnova make:migration create_todos_table
```

That writes `src/migrations/m<timestamp>_create_todos_table.rs`.
Replace the body with the schema for `todos`:

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
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Todos::Title).string().not_null())
                    .col(ColumnDef::new(Todos::Description).text().null())
                    .col(
                        ColumnDef::new(Todos::Done)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Todos::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Todos::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
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
    Done,
    CreatedAt,
    UpdatedAt,
}
```

Run it:

```bash
suprnova migrate
```

The `down` body lets `migrate:rollback` reverse the change later.

## Step 2: The model

A `#[suprnova::model]` struct *is* the Eloquent model — the macro
emits the SeaORM `Entity`, `Column`, and `ActiveModel` in an inner
module and gives the struct the query surface (`Todo::query()`,
`Todo::find`, `Todo::create`, `model.update`, `model.delete`,
auto-managed timestamps, lifecycle events). Create `src/models/todo.rs`:

```rust
use chrono::{DateTime, Utc};
use suprnova::model;

#[model(
    table = "todos",
    fillable = ["title", "description", "done"],
    timestamps,
)]
pub struct Todo {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub done: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Re-export the SeaORM types the macro emits in the inner `todo`
// module so call sites can reach for them without poking at the macro
// internals.
pub use todo::{ActiveModel, Column, Entity};
```

Wire the module into `src/models/mod.rs`:

```rust
pub mod todo;
```

The `fillable` list is the mass-assignment allowlist — only those
fields can be set via `Todo::create(attrs!{...})` and
`model.update(attrs!{...})`. Fields outside the list are guarded
against accidental writes from request input.

## Step 3: The form requests

Validation lives on a `#[request]` struct. `extract()` runs the
validator before the handler body sees the value; a failure short-
circuits to a 422 with the Laravel/Inertia error bag. Create
`src/requests.rs`:

```rust
use suprnova::request;

#[request]
pub struct CreateTodoRequest {
    #[validate(length(min = 1, max = 255, message = "title is required"))]
    pub title: String,

    #[validate(length(max = 1000))]
    pub description: Option<String>,
}

#[request]
pub struct UpdateTodoRequest {
    #[validate(length(min = 1, max = 255))]
    pub title: Option<String>,

    #[validate(length(max = 1000))]
    pub description: Option<String>,

    pub done: Option<bool>,
}
```

And register it in `src/lib.rs`:

```rust
pub mod requests;
```

The `#[request]` attribute expands to the equivalent of
`#[derive(serde::Deserialize, validator::Validate)] + impl FormRequest`,
so the struct fields are also the input schema. Optional fields
(`Option<T>`) are the right shape for partial updates: a missing key
in the JSON body deserialises to `None`, and the handler treats
`None` as "don't change this column".

## Step 4: The JSON:API resource

A resource is a `#[derive(Data)]` struct with `#[json_resource("type")]`.
The macro emits the `IntoJsonResource` impl that `Resource::single`,
`Resource::collection`, and `Resource::paginated` consume. The
resource's fields become the JSON:API `attributes` object — every
sparse-fieldset filter and `?include=` chain dispatches through this
type. Create `src/resources/todo_resource.rs`:

```rust
use crate::models::todo::Todo;
use suprnova::Data;
use validator::Validate;

#[derive(Debug, Clone, Data, Validate)]
#[json_resource("todos")]
pub struct TodoResource {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub done: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl From<Todo> for TodoResource {
    fn from(t: Todo) -> Self {
        Self {
            id: t.id,
            title: t.title,
            description: t.description,
            done: t.done,
            created_at: t.created_at.to_rfc3339(),
            updated_at: t.updated_at.to_rfc3339(),
        }
    }
}
```

Wire it in `src/resources/mod.rs`:

```rust
pub mod todo_resource;
```

And re-declare the module in `src/lib.rs`:

```rust
pub mod resources;
```

The `id` field supplies the JSON:API `id` member (stringified per
spec); every other field lands in `attributes` and is subject to
sparse-fieldset filtering — a request that names
`?fields[todos]=title,done` gets back only those two attributes,
without any handler-side work.

## Step 5: The controller

The `#[handler]` attribute classifies each parameter and generates
the matching extractor:

- `i64` — `FromParam` parses the named route param of the same name.
  Bad input (`/api/todos/abc`) short-circuits to 400.
- `CreateTodoRequest` / `UpdateTodoRequest` — `FromRequest`
  deserialises the body, runs validation, and 422s on failure.
- `Request` — passed through unchanged.

Loading the row goes through the Eloquent surface: `Todo::find_or_fail(id)`
returns a 404 when no row matches.

Create `src/controllers/todos.rs`:

```rust
use crate::models::todo::Todo;
use crate::requests::{CreateTodoRequest, UpdateTodoRequest};
use crate::resources::todo_resource::TodoResource;
use suprnova::{
    attrs, handler, LengthAwarePaginator, Model, Resource, Response,
};

// GET /api/todos?page=2
#[handler]
pub async fn index() -> Response {
    let page = Todo::query()
        .order_by_desc("created_at")
        .paginate(20)
        .await?;
    // Re-pack the paginator around `TodoResource` so the JSON:API
    // renderer sees resource objects, not raw models. The pagination
    // window (`total`, `per_page`, `current_page`) is preserved.
    let total = page.total;
    let per_page = page.per_page;
    let current_page = page.current_page;
    let resources: Vec<TodoResource> =
        page.data.into_iter().map(TodoResource::from).collect();
    let paginator = LengthAwarePaginator::new(resources, total, per_page, current_page)
        .with_path("/api/todos");
    Resource::paginated(paginator).render().await
}

// GET /api/todos/{todo}
#[handler]
pub async fn show(todo: i64) -> Response {
    let todo = Todo::find_or_fail(todo).await?;
    Resource::single(TodoResource::from(todo)).render().await
}

// POST /api/todos
#[handler]
pub async fn store(form: CreateTodoRequest) -> Response {
    let todo = Todo::create(attrs! {
        title: form.title,
        description: form.description,
        done: false,
    })
    .await?;
    Resource::single(TodoResource::from(todo))
        .created()           // 201
        .render()
        .await
}

// PUT /api/todos/{todo}
#[handler]
pub async fn update(todo: i64, form: UpdateTodoRequest) -> Response {
    let row = Todo::find_or_fail(todo).await?;

    let mut changes = attrs!();
    if let Some(title) = form.title {
        changes.insert("title", title.into());
    }
    if let Some(description) = form.description {
        changes.insert("description", description.into());
    }
    if let Some(done) = form.done {
        changes.insert("done", done.into());
    }
    let updated = row.update(changes).await?;
    Resource::single(TodoResource::from(updated)).render().await
}

// DELETE /api/todos/{todo}
#[handler]
pub async fn destroy(todo: i64) -> Response {
    Todo::find_or_fail(todo).await?.delete().await?;
    suprnova::json_response!({ "deleted": true })
}
```

Wire it in `src/controllers/mod.rs`:

```rust
pub mod todos;
```

The argument name must match the route placeholder — `{todo}` maps
to `todo: i64`. The macro parses the path segment via `FromParam`,
and the handler body then drives the Eloquent surface to load,
update, and delete the row.

## Step 6: The routes

`src/routes.rs`:

```rust
use crate::controllers::todos;
use suprnova::{delete, get, post, put, routes};

routes! {
    get!("/api/todos",           todos::index   ).name("todos.index"),
    get!("/api/todos/{todo}",    todos::show    ).name("todos.show"),
    post!("/api/todos",          todos::store   ).name("todos.store"),
    put!("/api/todos/{todo}",    todos::update  ).name("todos.update"),
    delete!("/api/todos/{todo}", todos::destroy ).name("todos.destroy"),
}
```

The `routes!` macro returns a configured `Router` that
`Application::routes(...)` consumes at boot.

## Step 7: Run it

```bash
suprnova serve --backend-only
```

### Create

```bash
curl -X POST http://localhost:8765/api/todos \
  -H "Content-Type: application/json" \
  -d '{"title": "Read JSON:API spec", "description": "All of it"}'
```

```json
{
  "data": {
    "type": "todos",
    "id": "1",
    "attributes": {
      "title": "Read JSON:API spec",
      "description": "All of it",
      "done": false,
      "created_at": "2026-05-30T12:00:00+00:00",
      "updated_at": "2026-05-30T12:00:00+00:00"
    }
  }
}
```

### List (paginated)

```bash
curl http://localhost:8765/api/todos
```

```json
{
  "data": [
    { "type": "todos", "id": "1", "attributes": { … } }
  ],
  "meta": {
    "pagination": {
      "total": 1,
      "per_page": 20,
      "current_page": 1,
      "last_page": 1
    }
  },
  "links": {
    "first": "?page=1",
    "last":  "?page=1",
    "prev":  null,
    "next":  null
  }
}
```

### Sparse fieldsets

```bash
curl 'http://localhost:8765/api/todos/1?fields[todos]=title,done'
```

```json
{
  "data": {
    "type": "todos",
    "id": "1",
    "attributes": { "title": "Read JSON:API spec", "done": false }
  }
}
```

The `IncludeMiddleware` parses `?fields[type]=...`, binds the filter
to a task-local, and `Resource::single` reads it during render —
the handler doesn't see the query parameter at all.

### Update

```bash
curl -X PUT http://localhost:8765/api/todos/1 \
  -H "Content-Type: application/json" \
  -d '{"done": true}'
```

A partial body works because every field in `UpdateTodoRequest` is
`Option<T>` — the handler only writes the keys that arrived.

### Delete

```bash
curl -X DELETE http://localhost:8765/api/todos/1
# {"deleted": true}
```

### Validation failure

```bash
curl -X POST http://localhost:8765/api/todos \
  -H "Content-Type: application/json" \
  -d '{"title": ""}'
```

```json
{
  "message": "The given data was invalid.",
  "errors": { "title": ["title is required"] },
  "request_id": "8f9e1a2b-…"
}
```

422 with the Laravel/Inertia error bag — the handler body never ran.

## Where each piece lives

| File | Role |
|------|------|
| `src/migrations/m*_create_todos_table.rs` | schema |
| `src/models/todo.rs` | `#[suprnova::model]` struct |
| `src/requests.rs` | `#[request]` form requests, validated by `extract()` |
| `src/resources/todo_resource.rs` | `#[derive(Data)]` + `#[json_resource("todos")]` |
| `src/controllers/todos.rs` | `#[handler]` functions |
| `src/routes.rs` | `routes!` registrations |

## Next

- [Eloquent](eloquent.md) — the full Model surface, query builder,
  `attrs!`, lifecycle events, soft deletes, relationships
- [Validation](validation.md) — `#[request]`, `validate!`, `Unique`,
  async hooks, cross-field rules
- [JSON:API Resources](eloquent-resources.md) — `?include=` chains,
  per-resource links/meta, `Maybe<T>` conditional attributes
- [Form Requests](requests.md) — `FormRequest` trait, content-type
  dispatch, `authorize(&Request)`
- [Controllers](controllers.md) — what `#[handler]` extracts and how
  route model binding works under the hood
