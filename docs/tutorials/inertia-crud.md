---
title: 'Build a Todo App with Inertia'
description: 'Create a full-stack CRUD app with React and Inertia.js'
icon: 'layer-group'
---

In this tutorial, you'll build a complete todo application with a React frontend using Inertia.js. You'll learn how to connect your Rust backend to a modern React UI.

## What We're Building

A full-featured todo app with:
- List all todos
- Create new todos
- Mark todos as complete
- Edit and delete todos
- All without writing a single API endpoint

## Prerequisites

```bash
suprnova new todo-app
cd todo-app
```

This creates a project with React and Inertia pre-configured.

## Step 1: Create the Migration

```bash
suprnova make:migration create_todos_table
```

Edit the migration:

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
                    .col(
                        ColumnDef::new(Todos::Completed)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(Todos::CreatedAt).timestamp().not_null())
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
    Completed,
    CreatedAt,
}
```

Run migrations:

```bash
suprnova migrate
suprnova db:sync
```

## Step 2: Create the Controller

```bash
suprnova make:controller todos
```

Edit `src/controllers/todos.rs`:

```rust
use suprnova::{handler, inertia_response, request, Request, Response, InertiaProps, redirect};
use suprnova::database::{Model, ModelMut};
use crate::models::todos::{Entity as Todos, ActiveModel, Model as Todo};
use sea_orm::ActiveValue::Set;

// Props for the index page
#[derive(InertiaProps)]
pub struct TodoIndexProps {
    pub todos: Vec<Todo>,
}

// Props for the create/edit page
#[derive(InertiaProps)]
pub struct TodoFormProps {
    pub todo: Option<Todo>,
}

// Request for creating/updating todos with validation
#[request]
#[derive(InertiaProps)]
pub struct TodoRequest {
    #[validate(length(min = 1, message = "Title is required"))]
    pub title: String,
}

// GET /todos
#[handler]
pub async fn index(_req: Request) -> Response {
    let todos = Todos::all().await.unwrap_or_default();
    inertia_response!("Todos/Index", TodoIndexProps { todos })
}

// GET /todos/create
#[handler]
pub async fn create(_req: Request) -> Response {
    inertia_response!("Todos/Create", TodoFormProps { todo: None })
}

// POST /todos - Uses Request for automatic validation
#[handler]
pub async fn store(form: TodoRequest) -> Response {
    // `form` is already validated - returns 422 if validation fails
    let new_todo = ActiveModel {
        title: Set(form.title),
        completed: Set(false),
        created_at: Set(chrono::Utc::now().naive_utc()),
        ..Default::default()
    };

    let _ = Todos::insert_one(new_todo).await;

    redirect("/todos")
}

// GET /todos/{todo}/edit - Route model binding automatically fetches the todo
#[handler]
pub async fn edit(todo: Todo) -> Response {
    inertia_response!("Todos/Edit", TodoFormProps { todo: Some(todo) })
}

// PUT /todos/{todo} - Route model binding automatically fetches the todo
#[handler]
pub async fn update(todo: Todo, form: TodoRequest) -> Response {
    let mut active: ActiveModel = todo.into();
    active.title = Set(form.title);
    let _ = Todos::update_one(active).await;
    redirect("/todos")
}

// POST /todos/{todo}/toggle - Route model binding automatically fetches the todo
#[handler]
pub async fn toggle(todo: Todo) -> Response {
    let mut active: ActiveModel = todo.clone().into();
    active.completed = Set(!todo.completed);
    let _ = Todos::update_one(active).await;
    redirect("/todos")
}

// DELETE /todos/{todo} - Route model binding automatically fetches the todo
#[handler]
pub async fn destroy(todo: Todo) -> Response {
    let _ = Todos::delete_by_pk(todo.id).await;
    redirect("/todos")
}
```

## Step 3: Define Routes

Edit `src/routes.rs`:

```rust
use suprnova::{get, post, put, delete, routes};
use crate::controllers::todos;

routes! {
    get!("/todos", todos::index).name("todos.index"),
    get!("/todos/create", todos::create).name("todos.create"),
    post!("/todos", todos::store).name("todos.store"),
    get!("/todos/{todo}/edit", todos::edit).name("todos.edit"),
    put!("/todos/{todo}", todos::update).name("todos.update"),
    post!("/todos/{todo}/toggle", todos::toggle).name("todos.toggle"),
    delete!("/todos/{todo}", todos::destroy).name("todos.destroy"),
}
```

## Step 4: Generate TypeScript Types

```bash
suprnova generate-types
```

This creates two files for end-to-end type safety:

### Props Types (`frontend/src/types/inertia-props.ts`)

```typescript
export interface Todo {
  id: number
  title: string
  completed: boolean
  created_at: string
}

export interface TodoIndexProps {
  todos: Todo[]
}

export interface TodoFormProps {
  todo: Todo | null
}

// FormRequest type for type-safe forms
export interface TodoRequest {
  title: string
}
```

### Route Types (`frontend/src/types/routes.ts`)

```typescript
import type { Method } from '@inertiajs/core';

export interface RouteConfig<TData = void> {
  url: string;
  method: Method;
  data?: TData;
}

// Path parameter types
export interface TodosToggleParams {
  todo: string;
}

export interface TodosEditParams {
  todo: string;
}

export interface TodosUpdateParams {
  todo: string;
}

export interface TodosDestroyParams {
  todo: string;
}

// Controller namespace - type-safe route helpers
export const controllers = {
  todos: {
    index: (): RouteConfig => ({ url: '/todos', method: 'get' }),
    create: (): RouteConfig => ({ url: '/todos/create', method: 'get' }),
    store: (): RouteConfig => ({ url: '/todos', method: 'post' }),
    edit: (params: TodosEditParams): RouteConfig => ({
      url: `/todos/${params.todo}/edit`,
      method: 'get',
    }),
    update: (params: TodosUpdateParams): RouteConfig => ({
      url: `/todos/${params.todo}`,
      method: 'put',
    }),
    toggle: (params: TodosToggleParams): RouteConfig => ({
      url: `/todos/${params.todo}/toggle`,
      method: 'post',
    }),
    destroy: (params: TodosDestroyParams): RouteConfig => ({
      url: `/todos/${params.todo}`,
      method: 'delete',
    }),
  },
} as const;
```

Now you have full type safety from backend routes to frontend navigation!

## Step 5: Create React Components

### Index Page

Create `frontend/src/pages/Todos/Index.tsx`:

```tsx
import { Link, router } from '@inertiajs/react'
import type { Todo, TodoIndexProps } from '../../types/inertia-props'
import { controllers } from '../../types/routes'

export default function TodoIndex({ todos }: TodoIndexProps) {
  const toggleTodo = (todo: Todo) => {
    router.visit(controllers.todos.toggle({ todo: todo.id.toString() }))
  }

  const deleteTodo = (todo: Todo) => {
    if (confirm('Are you sure?')) {
      router.visit(controllers.todos.destroy({ todo: todo.id.toString() }))
    }
  }

  return (
    <div className="max-w-2xl mx-auto p-8">
      <div className="flex justify-between items-center mb-6">
        <h1 className="text-3xl font-bold">My Todos</h1>
        <Link
          href={controllers.todos.create()}
          className="bg-blue-500 text-white px-4 py-2 rounded hover:bg-blue-600"
        >
          Add Todo
        </Link>
      </div>

      {/* Todo List */}
      {todos.length === 0 ? (
        <p className="text-gray-500 text-center py-8">No todos yet!</p>
      ) : (
        <ul className="space-y-3">
          {todos.map((todo) => (
            <li
              key={todo.id}
              className="flex items-center gap-3 p-4 bg-white rounded-lg shadow"
            >
              <input
                type="checkbox"
                checked={todo.completed}
                onChange={() => toggleTodo(todo)}
                className="w-5 h-5"
              />
              <span
                className={`flex-1 ${
                  todo.completed ? 'line-through text-gray-400' : ''
                }`}
              >
                {todo.title}
              </span>
              <Link
                href={controllers.todos.edit({ todo: todo.id.toString() })}
                className="text-blue-500 hover:underline"
              >
                Edit
              </Link>
              <button
                onClick={() => deleteTodo(todo)}
                className="text-red-500 hover:underline"
              >
                Delete
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
```

### Create Page

Create `frontend/src/pages/Todos/Create.tsx`:

```tsx
import { Form, Link, usePage } from '@inertiajs/react'
import { controllers } from '../../types/routes'

export default function TodoCreate() {
  const { errors } = usePage().props

  return (
    <div className="max-w-md mx-auto p-8">
      <h1 className="text-2xl font-bold mb-6">Create Todo</h1>

      {/* Pass the route object directly - Inertia v2+ UrlMethodPair compatible */}
      <Form action={controllers.todos.store()} className="space-y-4">
        {({ processing }) => (
          <>
            <div>
              <label className="block text-sm font-medium mb-1">Title</label>
              <input
                type="text"
                name="title"
                className="w-full border rounded px-3 py-2 focus:outline-none focus:ring-2 focus:ring-blue-500"
                placeholder="What needs to be done?"
                autoFocus
              />
              {errors?.title && (
                <p className="text-red-500 text-sm mt-1">{errors.title}</p>
              )}
            </div>

            <div className="flex gap-3">
              <button
                type="submit"
                disabled={processing}
                className="bg-blue-500 text-white px-4 py-2 rounded hover:bg-blue-600 disabled:opacity-50"
              >
                {processing ? 'Creating...' : 'Create Todo'}
              </button>
              <Link href={controllers.todos.index()} className="px-4 py-2 text-gray-600 hover:underline">
                Cancel
              </Link>
            </div>
          </>
        )}
      </Form>
    </div>
  )
}
```

### Edit Page

Create `frontend/src/pages/Todos/Edit.tsx`:

```tsx
import { Form, Link, usePage } from '@inertiajs/react'
import type { TodoFormProps } from '../../types/inertia-props'
import { controllers } from '../../types/routes'

export default function TodoEdit({ todo }: TodoFormProps) {
  const { errors } = usePage().props

  if (!todo) {
    return <div>Todo not found</div>
  }

  return (
    <div className="max-w-md mx-auto p-8">
      <h1 className="text-2xl font-bold mb-6">Edit Todo</h1>

      {/* Type-safe route with path params */}
      <Form action={controllers.todos.update({ todo: String(todo.id) })} className="space-y-4">
        {({ processing }) => (
          <>
            <div>
              <label className="block text-sm font-medium mb-1">Title</label>
              <input
                type="text"
                name="title"
                defaultValue={todo.title}
                className="w-full border rounded px-3 py-2 focus:outline-none focus:ring-2 focus:ring-blue-500"
                autoFocus
              />
              {errors?.title && (
                <p className="text-red-500 text-sm mt-1">{errors.title}</p>
              )}
            </div>

            <div className="flex gap-3">
              <button
                type="submit"
                disabled={processing}
                className="bg-blue-500 text-white px-4 py-2 rounded hover:bg-blue-600 disabled:opacity-50"
              >
                {processing ? 'Saving...' : 'Save Changes'}
              </button>
              <Link href={controllers.todos.index()} className="px-4 py-2 text-gray-600 hover:underline">
                Cancel
              </Link>
            </div>
          </>
        )}
      </Form>
    </div>
  )
}
```

## Step 6: Run the App

Start the development server:

```bash
suprnova serve
```

Visit `http://localhost:8080/todos` to see your todo app in action!

## How It Works

1. **No API Layer**: The Rust backend returns Inertia responses directly to React components
2. **End-to-End Type Safety**: TypeScript types are generated from Rust structs for both props AND routes
3. **Type-Safe Routes**: No magic strings - `controllers.todos.toggle({ id })` instead of `'/todos/${id}/toggle'`
4. **SPA Navigation**: Page transitions happen without full page reloads
5. **Forms**: `<Form>` component accepts route objects directly (Inertia v2+ `UrlMethodPair` compatible)
6. **Redirects**: After mutations, redirect to update the UI

## Key Inertia Features Used

| Feature | Description |
|---------|-------------|
| `inertia_response!` | Return page with props |
| `redirect()` | Navigate after mutations |
| `<Link href={controllers.x.y()}>` | Type-safe SPA navigation |
| `<Form action={controllers.x.y()}>` | Type-safe declarative form handling |
| `router.visit(controllers.x.y())` | Type-safe programmatic navigation |
| `controllers` object | Generated type-safe route helpers |

## Summary

You've built a complete CRUD application with end-to-end type safety:

- Database migrations and models
- Server-side controllers with Inertia responses
- React pages with type-safe props
- Type-safe routes with `controllers` object (no magic strings!)
- Forms with `<Form>` component and validation feedback
- Type-safe navigation

## Next Steps

- Add user authentication
- Implement optimistic updates
- Add toast notifications
- Deploy to production
