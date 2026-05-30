---
title: 'Database Overview'
description: 'Introduction to suprnova database layer powered by SeaORM'
icon: 'database'
---

suprnova provides a powerful database layer built on top of [SeaORM](https://www.sea-ql.org/SeaORM/), offering a Laravel-inspired experience for working with databases in Rust.

## Key Features

- **Multiple Database Support** - SQLite, PostgreSQL, and MySQL
- **Type-Safe Queries** - Leverage Rust's type system for compile-time query validation
- **Active Record Pattern** - `Model` and `ModelMut` traits for intuitive CRUD operations
- **DB Facade** - Simple static access to database connections
- **Automatic Entity Generation** - Generate Rust entities from your database schema
- **Migration System** - Laravel-style migrations for schema management

## Architecture

suprnova's database layer consists of several components:

```
┌─────────────────────────────────────────────────┐
│                 Your Application                │
├─────────────────────────────────────────────────┤
│     Model / ModelMut Traits (CRUD helpers)      │
├─────────────────────────────────────────────────┤
│           DB Facade (Connection Access)         │
├─────────────────────────────────────────────────┤
│       SeaORM (Query Builder & ORM Layer)        │
├─────────────────────────────────────────────────┤
│         Database Driver (SQLite/PG/MySQL)       │
└─────────────────────────────────────────────────┘
```

## Quick Example

Here's a complete example showing how to work with a database model:

```rust
use suprnova::{Request, Response, json_response};
use crate::models::todos::{Entity as Todos, Model as Todo};
use suprnova::database::{Model, ModelMut, DB};

// List all todos
pub async fn index(_req: Request) -> Response {
    let todos = Todos::all().await.unwrap();
    json_response!({ "todos": todos })
}

// Find a specific todo
pub async fn show(req: Request) -> Response {
    let id: i32 = req.param("id").unwrap().parse().unwrap();
    let todo = Todos::find_by_pk(id).await.unwrap();
    json_response!({ "todo": todo })
}

// Create a new todo
pub async fn store(_req: Request) -> Response {
    use crate::models::todos::ActiveModel;
    use sea_orm::ActiveValue::Set;

    let new_todo = ActiveModel {
        title: Set("New Task".to_string()),
        completed: Set(false),
        ..Default::default()
    };

    let created = Todos::insert_one(new_todo).await.unwrap();
    json_response!({ "todo": created })
}
```

## Getting Started

- [Configuration](/database/configuration) — Set up your database connection and configure connection pooling
- [Models](/database/models) — Learn about the Model and ModelMut traits for CRUD operations
- [Queries](/database/queries) — Build complex queries with SeaORM's query builder
- [Migrations](/database/migrations) — Manage your database schema with migrations

## Supported Databases

| Database | Status | Connection String |
|----------|--------|-------------------|
| SQLite | Supported | `sqlite:./database.db` |
| PostgreSQL | Supported | `postgres://user:pass@host/db` |
| MySQL | Supported | `mysql://user:pass@host/db` |

## Prerequisites

Before working with databases in suprnova, ensure you have:

1. A database server running (or use SQLite for local development)
2. The `DATABASE_URL` environment variable set in your `.env` file
3. Migrations created and run (see [Migrations](/database/migrations))

```env
# .env
DATABASE_URL=sqlite:./database.db
```
