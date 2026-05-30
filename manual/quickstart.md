# Quickstart

We're going to build a tiny "links" app — a single page that lists
URLs with titles, plus an API endpoint to post new ones. It exercises
routing, controllers, an Eloquent model, a migration, and an Inertia
page. If you can build this, you can build anything Suprnova does.

This assumes you've followed [Installation](installation.md) and have
the `suprnova` CLI on your `PATH`.

## 1. Scaffold

```bash
suprnova new links --frontend svelte --no-interaction
cd links
suprnova migrate
npm install
suprnova serve
```

Open `http://127.0.0.1:8000`. You should see the welcome page. Stop
the server (`Ctrl+C`) — we're going to add a feature.

## 2. Create the model and migration

There is no dedicated `make:model` command — models are regenerated
from the schema by `db:sync --regenerate-models` once the migration
runs. Start with the migration:

```bash
suprnova make:migration create_links_table
```

Open the new migration file under `src/migrations/`:

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.create_table(
            Table::create()
                .table(Alias::new("links"))
                .if_not_exists()
                .col(ColumnDef::new(Alias::new("id"))
                    .big_integer().primary_key().auto_increment().not_null())
                .col(ColumnDef::new(Alias::new("title")).string().not_null())
                .col(ColumnDef::new(Alias::new("url")).string().not_null())
                .col(ColumnDef::new(Alias::new("created_at"))
                    .timestamp_with_time_zone().not_null().default(Expr::current_timestamp()))
                .col(ColumnDef::new(Alias::new("updated_at"))
                    .timestamp_with_time_zone().not_null().default(Expr::current_timestamp()))
                .to_owned()
        ).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(Table::drop().table(Alias::new("links")).to_owned()).await
    }
}
```

Create the model by hand at `src/models/link.rs`:

```rust
use chrono::{DateTime, Utc};
use suprnova::{model, Model};

#[model(table = "links")]
pub struct Link {
    pub id: i64,
    pub title: String,
    pub url: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

Add `pub mod link;` to `src/models/mod.rs` so the new module is
reachable, then apply the migration and regenerate entities:

```bash
suprnova db:sync
```

`db:sync` runs pending migrations and regenerates SeaORM entities. The
combined step is the dev-loop default; in production you use plain
`suprnova migrate`.

## 3. Add a controller

`src/controllers/link.rs`:

```rust
use suprnova::{
    Data, InertiaProps, Model, Request, Response,
    handler, inertia_response, json_response,
};
use validator::Validate;
use crate::models::Link;

#[derive(InertiaProps)]
pub struct IndexProps {
    pub links: Vec<Link>,
}

pub async fn index(req: Request) -> Response {
    let links = Link::query().order_by_desc("created_at").get().await?;
    inertia_response!(&req, "Links/Index", IndexProps { links: links.into_vec() })
}

#[derive(Data, Validate)]
pub struct CreateLink {
    #[validate(length(min = 1, max = 200))]
    pub title: String,
    #[validate(url)]
    pub url: String,
}

#[handler]
pub async fn store(input: CreateLink) -> Response {
    let link = Link::create(suprnova::attrs! {
        title: input.title,
        url: input.url,
    }).await?;
    json_response!({ "link": link })
}
```

Register the controller module in `src/controllers/mod.rs`:

```rust
pub mod link;
```

## 4. Wire the routes

`src/routes.rs`:

```rust
use suprnova::{get, post, routes};
use crate::controllers;

routes! {
    get!("/", controllers::home::index).name("home"),

    // Links
    get!("/links", controllers::link::index).name("links.index"),
    post!("/links", controllers::link::store).name("links.store"),
}
```

## 5. Build the Inertia page

Create `frontend/src/pages/Links/Index.svelte` (for the Svelte starter):

```svelte
<script lang="ts">
    import { router } from '@inertiajs/svelte';

    let { links } = $props<{
        links: { id: number; title: string; url: string }[]
    }>();

    let title = $state('');
    let url = $state('');

    function submit(e: SubmitEvent) {
        e.preventDefault();
        router.post('/links', { title, url }, {
            onSuccess: () => { title = ''; url = ''; },
        });
    }
</script>

<div class="mx-auto max-w-2xl p-8">
    <h1 class="text-2xl font-bold">Links</h1>

    <form onsubmit={submit} class="mt-4 flex gap-2">
        <input bind:value={title} placeholder="Title"
               class="flex-1 rounded border p-2" />
        <input bind:value={url} placeholder="https://..."
               class="flex-1 rounded border p-2" />
        <button class="rounded bg-blue-600 px-4 py-2 text-white">Add</button>
    </form>

    <ul class="mt-8 space-y-2">
        {#each links as link}
            <li class="rounded border p-3">
                <a href={link.url} target="_blank"
                   class="text-blue-600 hover:underline">
                    {link.title}
                </a>
                <p class="text-sm text-gray-500">{link.url}</p>
            </li>
        {/each}
    </ul>
</div>
```

(Equivalent React and Vue starters give you the same shape with their
own templating — the Inertia bridge is identical.)

## 6. See it work

```bash
suprnova serve
```

Visit `http://127.0.0.1:8000/links`. Add a couple of links via the form.
They post to `/links`, the controller writes to the `links` table, and
the Inertia request re-fetches the index props. No JSON marshalling
glue — `InertiaProps` derived the wire format for you.

## What just happened

You touched eight files. Here's what they actually mean:

| File | Layer | Role |
|---|---|---|
| `src/migrations/m_create_links_table.rs` | Schema | Defines the `links` table |
| `src/models/link.rs` | Domain | One struct, four lines, full Eloquent model |
| `src/controllers/link.rs` | HTTP | Two handlers: `index` (page) and `store` (create) |
| `src/routes.rs` | Router | Wires URLs to handlers via `routes!` |
| `src/controllers/mod.rs` | Wiring | Re-exports the new controller module |
| `frontend/src/pages/Links/Index.svelte` | Frontend | The page Inertia renders |
| (existing) `bootstrap.rs` | Boot | Where you'd register observers/services for this feature |
| (existing) `.env` | Config | DB URL, ports, secrets |

That's the standard rhythm: migration → model → controller →
route → frontend page. Every feature, no matter how big, decomposes
into those steps.

## What to read next

You've done a full vertical slice. The next things you'll reach for:

- [Routing](routing.md) — grouping, middleware, named routes, signed
  URLs, resource routing
- [Validation](validation.md) — what `#[derive(Validate)]` gives you
- [Eloquent](eloquent.md) — relationships, scopes, observers, soft
  deletes, the full query builder surface
- [Inertia + Frontend](frontend.md) — partial reloads, typed props,
  TypeScript type generation
- [Authentication](authentication.md) — the auth scaffolding the
  starter shipped
- [Console](console.md) — `cargo run --bin console <subcommand>` and
  writing your own commands

Or browse [`documentation.md`](documentation.md) for the full TOC.
