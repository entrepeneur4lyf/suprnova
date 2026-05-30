# Build a Todo App with Inertia

A vertical slice of Suprnova that exercises the full stack: a migration, a
`#[suprnova::model]`, Inertia-rendered Svelte 5 pages, route model binding,
form validation, and type-safe route helpers generated from `routes.rs`.
Work through this once and the project loop — migration, model, controller,
route, page — becomes muscle memory.

This assumes you've followed [Installation](installation.md) and have the
`suprnova` CLI on your `PATH`. The scaffolder defaults to Svelte 5, which
is what this tutorial uses.

## What you'll build

A todo page with create, list, toggle-complete, edit, and delete. No
separate JSON API: Inertia serialises props and the Svelte page consumes
them as `$props()` — the same struct flows from Rust to the browser.

## 1. Scaffold

```bash
suprnova new todo-app --frontend svelte --no-interaction
cd todo-app
npm install
```

## 2. Migration

```bash
suprnova make:migration create_todos_table
```

Open the new migration under `src/migrations/`:

```rust
use suprnova::sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("todos"))
                    .if_not_exists()
                    .col(ColumnDef::new(Alias::new("id"))
                        .big_integer().primary_key().auto_increment().not_null())
                    .col(ColumnDef::new(Alias::new("title")).string().not_null())
                    .col(ColumnDef::new(Alias::new("completed"))
                        .boolean().not_null().default(false))
                    .col(ColumnDef::new(Alias::new("created_at"))
                        .timestamp_with_time_zone().not_null()
                        .default(Expr::current_timestamp()))
                    .col(ColumnDef::new(Alias::new("updated_at"))
                        .timestamp_with_time_zone().not_null()
                        .default(Expr::current_timestamp()))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Alias::new("todos")).to_owned())
            .await
    }
}
```

Both `created_at` and `updated_at` are present because the model in the
next step uses `timestamps`, which expects both columns and auto-manages
them. Then run migrations and regenerate entities:

```bash
suprnova db:sync
```

`db:sync` runs pending migrations and refreshes the SeaORM entity layer
the `#[suprnova::model]` macro relies on.

## 3. Model

Create `src/models/todo.rs`:

```rust
use chrono::{DateTime, Utc};
use suprnova::model;

#[model(
    table = "todos",
    fillable = ["title", "completed"],
    timestamps,
)]
pub struct Todo {
    pub id: i64,
    pub title: String,
    pub completed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// The model macro emits an inner `todo` module with the SeaORM
// Entity, ActiveModel, Column, and Model types. Re-export the ones
// you want to reach from outside the file.
pub use todo::{ActiveModel, Column, Entity};
```

Wire the new module in `src/models/mod.rs`:

```rust
pub mod todo;
```

The `fillable` list gates mass assignment; `timestamps` auto-manages
`created_at` / `updated_at` on every save. The user-facing `Todo` struct
is the type you'll work with in handlers; the inner `todo::Model` is the
SeaORM shape that route model binding fetches.

## 4. Controller

```bash
suprnova make:controller todo
```

Open `src/controllers/todo.rs`:

```rust
use suprnova::{
    attrs, handler, inertia_response, redirect_to, request, InertiaProps,
    Model, Request, Response,
};

use crate::models::todo::{todo, Todo};

#[derive(InertiaProps)]
pub struct TodoIndexProps {
    pub todos: Vec<Todo>,
}

#[derive(InertiaProps)]
pub struct TodoFormProps {
    pub todo: Option<Todo>,
}

#[request]
pub struct TodoForm {
    #[validate(length(min = 1, max = 200, message = "Title is required"))]
    pub title: String,
}

#[handler]
pub async fn index(_req: Request) -> Response {
    let todos = Todo::all().await?.into_vec();
    inertia_response!("Todos/Index", TodoIndexProps { todos })
}

#[handler]
pub async fn create(_req: Request) -> Response {
    inertia_response!("Todos/Create", TodoFormProps { todo: None })
}

#[handler]
pub async fn store(form: TodoForm) -> Response {
    Todo::create(attrs! {
        title: form.title,
        completed: false,
    })
    .await?;
    redirect_to("/todos").into()
}

#[handler]
pub async fn edit(todo: todo::Model) -> Response {
    let todo: Todo = todo.into();
    inertia_response!("Todos/Edit", TodoFormProps { todo: Some(todo) })
}

#[handler]
pub async fn update(todo: todo::Model, form: TodoForm) -> Response {
    let todo: Todo = todo.into();
    todo.update(attrs! { title: form.title }).await?;
    redirect_to("/todos").into()
}

#[handler]
pub async fn toggle(todo: todo::Model) -> Response {
    let todo: Todo = todo.into();
    let next = !todo.completed;
    todo.update(attrs! { completed: next }).await?;
    redirect_to("/todos").into()
}

#[handler]
pub async fn destroy(todo: todo::Model) -> Response {
    let todo: Todo = todo.into();
    todo.delete().await?;
    redirect_to("/todos").into()
}
```

A few things to notice:

- **Route model binding is automatic.** Declaring `todo: todo::Model` tells
  the `#[handler]` macro to look up `{todo}` in the route path, fetch the
  SeaORM row by primary key, and 404 if it's missing. The parameter name
  must match the route placeholder.
- **The macro hands you `todo::Model`; the Eloquent surface lives on
  `Todo`.** The two are bridged by a `From` impl emitted by
  `#[suprnova::model]`, so `let todo: Todo = todo.into();` is the
  one-line conversion. `Todo` is the type that carries `update`,
  `delete`, and the rest of the user-facing API.
- **`#[request]` covers validation.** Adding it to a struct generates
  `Deserialize`, `Validate`, and `FormRequest` — the framework rejects
  malformed input with a 422 before your handler runs. There's no need
  to also derive `InertiaProps` on a request DTO; that derive is for
  *outgoing* page props.
- **Mass assignment goes through `attrs!`.** `Todo::create(attrs! { ... })`
  and `todo.update(attrs! { ... })` route through the fillable filter, so
  fields not in the model's `fillable` list silently drop instead of
  bypassing the guard.
- **`update` and `delete` consume `self`.** That's why `toggle` reads
  `!todo.completed` into a local before calling `todo.update(...)`.

Register the new controller module in `src/controllers/mod.rs`:

```rust
pub mod todo;
```

### Why Suprnova diverges

In Laravel, the same controller would normally return JSON for an API or
a Blade view for a server-rendered page. Suprnova returns Inertia
responses for both initial loads and SPA navigations — the framework
detects the `X-Inertia` header and serves HTML or JSON accordingly,
without a parallel API layer. You write your handlers once, your
frontend stays a real SPA, and there's no second router to keep in
sync. See [Inertia Responses](frontend-inertia-responses.md) for the
mechanics.

## 5. Routes

`src/routes.rs`:

```rust
use suprnova::{delete, get, post, put, routes};

use crate::controllers::todo;

routes! {
    get!("/todos", todo::index).name("todos.index"),
    get!("/todos/create", todo::create).name("todos.create"),
    post!("/todos", todo::store).name("todos.store"),
    get!("/todos/{todo}/edit", todo::edit).name("todos.edit"),
    put!("/todos/{todo}", todo::update).name("todos.update"),
    post!("/todos/{todo}/toggle", todo::toggle).name("todos.toggle"),
    delete!("/todos/{todo}", todo::destroy).name("todos.destroy"),
}
```

The `{todo}` placeholder is what route model binding hooks onto: it has
to match the handler parameter name (`todo`), and it has to match the
SeaORM model's primary-key type (here, `i64`). The optional `.name(...)`
suffix is what the route-type generator in the next step uses to build
the frontend helpers.

## 6. Generate TypeScript types

```bash
suprnova generate-types
```

`generate-types` does two things in one pass:

1. Walks every `#[derive(InertiaProps)]` struct in `src/` and writes them
   to `frontend/src/types/inertia-props.ts`.
2. Walks `src/routes.rs` and writes typed URL builders for every named
   route to `frontend/src/types/routes.ts`.

The route helpers come out as a nested object — `controllers.todos.toggle({ todo: "1" })`
returns a `{ url, method }` pair that Inertia 3's `Link` and `router`
accept directly. Path parameters are typed; the compiler catches a
missing `todo` argument before the page hits the browser.

You don't have to edit these files. Re-run `suprnova generate-types`
whenever you add or rename props/routes, or pass `--watch` to keep them
in sync as you go.

## 7. Pages

Each page lives under `frontend/src/pages/Todos/`. The names match the
strings you pass to `inertia_response!`, so `inertia_response!("Todos/Index", ...)`
resolves to `frontend/src/pages/Todos/Index.svelte`.

### Index

`frontend/src/pages/Todos/Index.svelte`:

```svelte
<script lang="ts">
  import { Link, router } from '@inertiajs/svelte'
  import type { Todo, TodoIndexProps } from '../../types/inertia-props'
  import { controllers } from '../../types/routes'

  let { todos }: TodoIndexProps = $props()

  function toggle(todo: Todo) {
    router.visit(controllers.todos.toggle({ todo: String(todo.id) }))
  }

  function remove(todo: Todo) {
    if (confirm('Delete this todo?')) {
      router.visit(controllers.todos.destroy({ todo: String(todo.id) }))
    }
  }
</script>

<div class="mx-auto max-w-2xl p-8">
  <div class="mb-6 flex items-center justify-between">
    <h1 class="text-2xl font-bold">My Todos</h1>
    <Link
      href={controllers.todos.create()}
      class="rounded bg-blue-600 px-4 py-2 text-white hover:bg-blue-700"
    >
      Add todo
    </Link>
  </div>

  {#if todos.length === 0}
    <p class="text-center text-gray-500">No todos yet.</p>
  {:else}
    <ul class="space-y-2">
      {#each todos as todo (todo.id)}
        <li class="flex items-center gap-3 rounded border p-3">
          <input
            type="checkbox"
            checked={todo.completed}
            onchange={() => toggle(todo)}
            class="h-5 w-5"
          />
          <span class={todo.completed ? 'flex-1 text-gray-400 line-through' : 'flex-1'}>
            {todo.title}
          </span>
          <Link
            href={controllers.todos.edit({ todo: String(todo.id) })}
            class="text-blue-600 hover:underline"
          >
            Edit
          </Link>
          <button
            onclick={() => remove(todo)}
            class="text-red-600 hover:underline"
          >
            Delete
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>
```

### Create

`frontend/src/pages/Todos/Create.svelte`:

```svelte
<script lang="ts">
  import { Link, useForm } from '@inertiajs/svelte'
  import { controllers } from '../../types/routes'

  const form = useForm({ title: '' })

  function submit(e: SubmitEvent) {
    e.preventDefault()
    form.post(controllers.todos.store().url)
  }
</script>

<div class="mx-auto max-w-md p-8">
  <h1 class="mb-6 text-2xl font-bold">Create todo</h1>

  <form onsubmit={submit} class="space-y-4">
    <div>
      <label for="title" class="mb-1 block text-sm font-medium">Title</label>
      <input
        id="title"
        type="text"
        bind:value={form.title}
        class="w-full rounded border px-3 py-2"
        placeholder="What needs to be done?"
      />
      {#if form.errors?.title}
        <p class="mt-1 text-sm text-red-600">{form.errors.title}</p>
      {/if}
    </div>

    <div class="flex gap-3">
      <button
        type="submit"
        disabled={form.processing}
        class="rounded bg-blue-600 px-4 py-2 text-white hover:bg-blue-700 disabled:opacity-50"
      >
        {form.processing ? 'Creating...' : 'Create'}
      </button>
      <Link
        href={controllers.todos.index()}
        class="px-4 py-2 text-gray-600 hover:underline"
      >
        Cancel
      </Link>
    </div>
  </form>
</div>
```

### Edit

`frontend/src/pages/Todos/Edit.svelte`:

```svelte
<script lang="ts">
  import { Link, useForm } from '@inertiajs/svelte'
  import type { TodoFormProps } from '../../types/inertia-props'
  import { controllers } from '../../types/routes'

  const props: TodoFormProps = $props()
  const todo = props.todo!

  const form = useForm({ title: todo.title })

  function submit(e: SubmitEvent) {
    e.preventDefault()
    form.put(controllers.todos.update({ todo: String(todo.id) }).url)
  }
</script>

<div class="mx-auto max-w-md p-8">
  <h1 class="mb-6 text-2xl font-bold">Edit todo</h1>

  <form onsubmit={submit} class="space-y-4">
    <div>
      <label for="title" class="mb-1 block text-sm font-medium">Title</label>
      <input
        id="title"
        type="text"
        bind:value={form.title}
        class="w-full rounded border px-3 py-2"
      />
      {#if form.errors?.title}
        <p class="mt-1 text-sm text-red-600">{form.errors.title}</p>
      {/if}
    </div>

    <div class="flex gap-3">
      <button
        type="submit"
        disabled={form.processing}
        class="rounded bg-blue-600 px-4 py-2 text-white hover:bg-blue-700 disabled:opacity-50"
      >
        {form.processing ? 'Saving...' : 'Save'}
      </button>
      <Link
        href={controllers.todos.index()}
        class="px-4 py-2 text-gray-600 hover:underline"
      >
        Cancel
      </Link>
    </div>
  </form>
</div>
```

The equivalent React 19 and Vue 3.5 starters take the same props through
their own templating — the backend doesn't change.

## 8. Run it

```bash
suprnova serve
```

Visit `http://127.0.0.1:8000/todos`, add a few rows, toggle them, edit
one, delete another. The page transitions happen through Inertia — no
full reload — and every form submission validates server-side before
the redirect lands.

## What just happened

| Layer | File | What it does |
|---|---|---|
| Schema | `src/migrations/m_create_todos_table.rs` | Creates the `todos` table |
| Model | `src/models/todo.rs` | The user-facing `Todo` struct + the inner SeaORM module |
| HTTP | `src/controllers/todo.rs` | Seven `#[handler]`s, including route model binding |
| Router | `src/routes.rs` | Named routes that drive the generated route helpers |
| Props | `frontend/src/types/inertia-props.ts` | Generated from `#[derive(InertiaProps)]` |
| Routes | `frontend/src/types/routes.ts` | Generated from named routes in `routes.rs` |
| Pages | `frontend/src/pages/Todos/*.svelte` | The three Svelte 5 pages that consume the props |

That's the standard Suprnova feature loop: migration -> model -> controller
-> route -> page, with `suprnova generate-types` regenerating the
TypeScript bridge whenever you reshape props or rename a route.

## Next

- [Eloquent](eloquent.md) — `attrs!`, the query builder, casts, scopes,
  observers
- [Validation](validation.md) — what `#[request]` and `#[derive(Validate)]`
  give you
- [Routing](routing.md) — named routes, route model binding, resource
  routing, signed URLs
- [Inertia Responses](frontend-inertia-responses.md) — `inertia_response!`,
  partial reloads, shared props
- [Authentication](authentication.md) — adding per-user todos with the
  starter's session auth
