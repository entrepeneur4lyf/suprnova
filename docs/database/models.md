---
title: 'Models & Entities'
description: 'Working with database models using an Eloquent-like fluent API'
icon: 'cube'
---

suprnova provides a Laravel/Eloquent-inspired fluent API for working with database models, making common CRUD operations simple and intuitive.

## Entity Structure

suprnova uses SeaORM entities under the hood. When you suprnova `suprnova db:sync`, two files are generated for each table:

1. **Auto-generated entity** (`src/models/entities/{table}.rs`) - SeaORM entity definition, regenerated on sync
2. **User model** (`src/models/{table}.rs`) - Your custom code with the fluent API, never overwritten

```rust
// src/models/todos.rs (generated with fluent API)
pub use super::entities::todos::*;

use suprnova::database::{ModelMut, QueryBuilder};
use sea_orm::{entity::prelude::*, Set};

/// Type alias for convenient access
pub type Todo = Model;

impl Model {
    pub fn query() -> QueryBuilder<Entity> { ... }
    pub fn create() -> TodoBuilder { ... }
    pub fn set_title(mut self, value: impl Into<String>) -> Self { ... }
    pub async fn update(self) -> Result<Self, FrameworkError> { ... }
    pub async fn delete(self) -> Result<u64, FrameworkError> { ... }
}

pub struct TodoBuilder { ... }
```

## Generating Models

Use the `db:sync` command to generate models from your database:

```bash
# Run migrations and generate models
suprnova db:sync

# Skip migrations, just regenerate entities
suprnova db:sync --skip-migrations

# Regenerate all model files (including user models with fluent API)
suprnova db:sync --regenerate-models
```

> **Warning:**
>
> The `--regenerate-models` flag will overwrite your custom model files. Use with caution if you've added custom methods.


## Eloquent-like Fluent API

The generated models provide a fluent, chainable API similar to Laravel's Eloquent.

### Querying Records

Use `Model::query()` to start a fluent query builder:

```rust
use crate::models::todos::{Todo, Column};

// Get all records
let todos = Todo::query().all().await?;

// Filter with conditions
let active_todos = Todo::query()
    .filter(Column::Completed.eq(false))
    .all()
    .await?;

// Chain multiple filters
let urgent = Todo::query()
    .filter(Column::Priority.eq("high"))
    .filter(Column::Completed.eq(false))
    .all()
    .await?;

// Get first record
let first = Todo::query().first().await?; // Option<Model>

// Get first or fail
let todo = Todo::query()
    .filter(Column::Id.eq(1))
    .first_or_fail()
    .await?;

// Count records
let count = Todo::query()
    .filter(Column::Completed.eq(true))
    .count()
    .await?;

// Check existence
let has_incomplete = Todo::query()
    .filter(Column::Completed.eq(false))
    .exists()
    .await?;

// Ordering and pagination
let recent = Todo::query()
    .order_by_desc(Column::CreatedAt)
    .limit(10)
    .offset(0)
    .all()
    .await?;
```

### Creating Records

Use `Model::create()` to get a fluent builder for creating new records:

```rust
use crate::models::todos::Todo;

// Create with fluent setters
let todo = Todo::create()
    .set_title("Learn suprnova")
    .set_description("Build something awesome")
    .insert()
    .await?;

println!("Created todo with ID: {}", todo.id);
```

### Updating Records

Chain setters on an existing model and call `update()`:

```rust
use crate::models::todos::{Todo, Column};

// Find and update
let todo = Todo::query()
    .filter(Column::Id.eq(1))
    .first_or_fail()
    .await?;

let updated = todo
    .set_title("Updated title")
    .update()
    .await?;
```

### Deleting Records

Call `delete()` on a model instance:

```rust
let todo = Todo::query()
    .filter(Column::Id.eq(1))
    .first_or_fail()
    .await?;

todo.delete().await?;
```

## Complete Example

Here's a complete CRUD controller using the fluent API:

```rust
// src/controllers/todos.rs
use suprnova::{handler, json_response, Request, Response};
use crate::models::todos::{Todo, Column};

#[handler]
pub async fn index(_req: Request) -> Response {
    match Todo::query().all().await {
        Ok(todos) => json_response!({ "todos": todos }),
        Err(e) => json_response!({ "error": e.to_string() }),
    }
}

#[handler]
pub async fn show(req: Request) -> Response {
    let id: i32 = req.param("id").unwrap().parse().unwrap();

    match Todo::query().filter(Column::Id.eq(id)).first_or_fail().await {
        Ok(todo) => json_response!({ "todo": todo }),
        Err(_) => json_response!({ "error": "Todo not found" }),
    }
}

#[handler]
pub async fn store(req: Request) -> Response {
    let title = "New Todo".to_string();

    match Todo::create()
        .set_title(title)
        .set_description("A new todo item")
        .insert()
        .await
    {
        Ok(todo) => json_response!({ "todo": todo }),
        Err(e) => json_response!({ "error": e.to_string() }),
    }
}

#[handler]
pub async fn update(req: Request) -> Response {
    let id: i32 = req.param("id").unwrap().parse().unwrap();

    let todo = Todo::query()
        .filter(Column::Id.eq(id))
        .first_or_fail()
        .await
        .unwrap();

    match todo.set_title("Updated title").update().await {
        Ok(updated) => json_response!({ "todo": updated }),
        Err(e) => json_response!({ "error": e.to_string() }),
    }
}

#[handler]
pub async fn destroy(req: Request) -> Response {
    let id: i32 = req.param("id").unwrap().parse().unwrap();

    let todo = Todo::query()
        .filter(Column::Id.eq(id))
        .first_or_fail()
        .await
        .unwrap();

    match todo.delete().await {
        Ok(_) => json_response!({ "deleted": true }),
        Err(e) => json_response!({ "error": e.to_string() }),
    }
}
```

## Model and ModelMut Traits

The fluent API is built on top of two core traits that you can also use directly:

### The Model Trait

Provides read-only operations on the Entity:

```rust
use suprnova::database::Model;
use crate::models::todos::Entity as Todos;

// These methods are available on Entity
let todos = Todos::all().await?;
let todo = Todos::find_by_pk(1).await?;
let todo = Todos::find_or_fail(1).await?;
let first = Todos::first().await?;
let count = Todos::count_all().await?;
let exists = Todos::exists_any().await?;
```

> **Note:**
>
> Implementing `suprnova::database::Model` also enables [Route Model Binding](/core/routing#route-model-binding), allowing you to use `Model` types directly as handler parameters for automatic database lookup.


### The ModelMut Trait

Provides write operations:

```rust
use suprnova::database::ModelMut;
use crate::models::todos::{Entity as Todos, ActiveModel};
use sea_orm::ActiveValue::Set;

// Insert
let new_todo = ActiveModel {
    title: Set("Learn suprnova".to_string()),
    ..Default::default()
};
let created = Todos::insert_one(new_todo).await?;

// Update
let mut active: ActiveModel = existing_todo.into();
active.title = Set("Updated".to_string());
let updated = Todos::update_one(active).await?;

// Delete
let deleted = Todos::delete_by_pk(1).await?;

// Save (insert or update)
let saved = Todos::save_one(active_model).await?;
```

## Working with ActiveModel

For advanced use cases, you can work directly with SeaORM's `ActiveModel`:

```rust
use sea_orm::ActiveValue::{Set, NotSet, Unchanged};

// Creating a new record
let new_todo = ActiveModel {
    id: NotSet,                        // Auto-increment
    title: Set("New task".to_string()),
    completed: Set(false),
    created_at: NotSet,                // Use database default
    updated_at: NotSet,
};

// Updating specific fields
let mut update_todo = ActiveModel {
    id: Unchanged(1),                  // Keep the same ID
    title: Set("Updated title".to_string()),
    completed: Unchanged(false),       // Don't change this field
    ..Default::default()
};
```

### ActiveValue States

| State | Meaning | SQL Behavior |
|-------|---------|--------------|
| `Set(value)` | Set to this value | Included in INSERT/UPDATE |
| `NotSet` | Not specified | Excluded (uses default) |
| `Unchanged(value)` | Keep current value | Excluded from UPDATE |

## Model Relationships

Define relationships in your model file:

```rust
// src/models/posts.rs
impl Entity {
    pub fn belongs_to_user() -> RelationDef {
        Entity::belongs_to(super::users::Entity)
            .from(Column::UserId)
            .to(super::users::Column::Id)
            .into()
    }
}

impl Related<super::users::Entity> for Entity {
    fn to() -> RelationDef {
        Self::belongs_to_user()
    }
}
```

### Loading Relationships

```rust
use sea_orm::EntityTrait;
use suprnova::database::DB;
use crate::models::{posts, users};

// Find post with its user
let post_with_user = posts::Entity::find_by_id(1)
    .find_also_related(users::Entity)
    .one(DB::connection()?.inner())
    .await?;

if let Some((post, Some(user))) = post_with_user {
    println!("Post: {}, Author: {}", post.title, user.name);
}
```

## API Summary

### Fluent API (on Model)

| Method | Description |
|--------|-------------|
| `Model::query()` | Start a fluent query builder |
| `Model::create()` | Start a fluent builder for creating |
| `model.set_field(value)` | Set a field (chainable) |
| `model.update()` | Save changes to database |
| `model.delete()` | Delete the record |

### QueryBuilder Methods

| Method | Description |
|--------|-------------|
| `.filter(condition)` | Add a filter condition |
| `.order_by_asc(column)` | Order ascending |
| `.order_by_desc(column)` | Order descending |
| `.limit(n)` | Limit results |
| `.offset(n)` | Skip results |
| `.all()` | Execute and get all results |
| `.first()` | Get first result (Option) |
| `.first_or_fail()` | Get first or error |
| `.count()` | Count matching records |
| `.exists()` | Check if any match |

### Entity Trait Methods

| Trait | Method | Description |
|-------|--------|-------------|
| `Model` | `all()` | Get all records |
| `Model` | `find_by_pk(id)` | Find by primary key |
| `Model` | `find_or_fail(id)` | Find or error |
| `Model` | `first()` | Get first record |
| `Model` | `count_all()` | Count all records |
| `Model` | `exists_any()` | Check if any exist |
| `ModelMut` | `insert_one(model)` | Insert a record |
| `ModelMut` | `update_one(model)` | Update a record |
| `ModelMut` | `save_one(model)` | Insert or update |
| `ModelMut` | `delete_by_pk(id)` | Delete by primary key |
