# Page Components

A page is the unit Inertia ships across the wire. The Rust controller picks a
component name and a typed props struct; the Vite-bundled frontend resolves
that name to a file in `frontend/src/pages/` and renders it with the props as
arguments. The framework is framework-agnostic — Suprnova ships first-class
starters for Svelte 5, React 19, and Vue 3.5, and the page contract is the
same shape in all three.

## The contract

A controller returns an Inertia response naming a component:

```rust
use suprnova::{InertiaProps, Request, Response, inertia_response};

#[derive(InertiaProps)]
pub struct HomeProps {
    pub title: String,
    pub message: String,
}

pub async fn index(req: Request) -> Response {
    inertia_response!(&req, "Home", HomeProps {
        title: "Welcome".to_string(),
        message: "Hello from Suprnova!".to_string(),
    })
}
```

The string `"Home"` is resolved against `frontend/src/pages/Home.<ext>`. The
extension depends on which starter you scaffolded:

| Starter | Extension | Default? |
|---|---|---|
| Svelte 5 | `.svelte` | yes |
| React 19 | `.tsx` | — |
| Vue 3.5 | `.vue` | — |

The macro validates at compile time that the file exists, so a typo or a
deleted page fails `cargo check` instead of 500-ing in production.

## Directory layout

Whichever framework you picked, pages live under `frontend/src/pages/` and
the component name in `inertia_response!` is the file path relative to that
directory, without the extension. Forward slashes work the same on all
platforms.

```
frontend/src/pages/
├── Home.svelte                 # inertia_response!(&req, "Home", ...)
├── About.svelte                # inertia_response!(&req, "About", ...)
├── Users/
│   ├── Index.svelte            # inertia_response!(&req, "Users/Index", ...)
│   ├── Show.svelte             # inertia_response!(&req, "Users/Show", ...)
│   └── Edit.svelte             # inertia_response!(&req, "Users/Edit", ...)
├── Posts/
│   ├── Index.svelte            # inertia_response!(&req, "Posts/Index", ...)
│   └── Show.svelte             # inertia_response!(&req, "Posts/Show", ...)
└── auth/
    ├── Login.svelte            # inertia_response!(&req, "auth/Login", ...)
    └── Register.svelte         # inertia_response!(&req, "auth/Register", ...)
```

The convention is `Index` for collection pages, `Show` / `Edit` / `Create` for
single-item pages, and a lowercase subdirectory like `auth/` for grouped
feature pages. Capitalisation in the component name must match the file name
exactly — Vite's `import.meta.glob` is case-sensitive.

## Generating a page

The CLI's `make:inertia` generator drops a starter component into the right
location and uses the syntax for whichever frontend the project is using:

```bash
suprnova make:inertia Dashboard
```

The generator reads `SUPRNOVA_FRONTEND` from your `.env` (defaulting to
Svelte), picks the matching extension, and appends `Page` to the component
name if it's not already there. So the command above creates one of:

- `frontend/src/pages/DashboardPage.svelte`
- `frontend/src/pages/DashboardPage.tsx`
- `frontend/src/pages/DashboardPage.vue`

The console output prints the matching `inertia_response!` call you should
paste into your controller.

To skip the suffix and own the name, pass the full name:

```bash
suprnova make:inertia DashboardPage   # creates DashboardPage.<ext>
```

To generate a typed props struct on the Rust side instead, pass `--data`:

```bash
suprnova make:inertia Dashboard --data
# Creates app/src/props/dashboard.rs with #[derive(Data, Validate)]
```

## A page in each starter

The same `inertia_response!(&req, "Home", HomeProps { ... })` on the backend
maps to one of these page files on the frontend. Props arrive as typed
arguments via the generated `inertia-props.ts` types.

### Svelte 5

Runes-on. Props arrive via `$props()`:

```svelte
<!-- frontend/src/pages/Home.svelte -->
<script lang="ts">
  import type { HomeProps } from '../types/inertia-props'

  let { title, message }: HomeProps = $props()
</script>

<div class="font-sans p-8 max-w-xl mx-auto">
  <h1 class="text-3xl font-bold">{title}</h1>
  <p class="mt-2">{message}</p>
</div>
```

### React 19

Standard function component. Props arrive as the first argument:

```tsx
// frontend/src/pages/Home.tsx
import type { HomeProps } from '../types/inertia-props'

export default function Home({ title, message }: HomeProps) {
  return (
    <div className="font-sans p-8 max-w-xl mx-auto">
      <h1 className="text-3xl font-bold">{title}</h1>
      <p className="mt-2">{message}</p>
    </div>
  )
}
```

### Vue 3.5

`<script setup lang="ts">` with `defineProps`. Props are accessed directly
in the template:

```vue
<!-- frontend/src/pages/Home.vue -->
<script setup lang="ts">
import type { HomeProps } from '../types/inertia-props'

defineProps<HomeProps>()
</script>

<template>
  <div class="font-sans p-8 max-w-xl mx-auto">
    <h1 class="text-3xl font-bold">{{ title }}</h1>
    <p class="mt-2">{{ message }}</p>
  </div>
</template>
```

## Navigation between pages

Each starter ships the Inertia v3 adapter for its framework. The exports are
the same: `Link` for declarative navigation, `router` for programmatic
navigation, `usePage` (or `page`) for shared props, `Form` and `useForm` for
form handling.

### Svelte 5

```svelte
<script lang="ts">
  import { Link, router } from '@inertiajs/svelte'

  function gotoPosts() {
    router.visit('/posts')
  }
</script>

<Link href="/posts">All posts</Link>
<Link href="/posts/42" method="delete" as="button">Delete</Link>

<button onclick={gotoPosts}>Visit programmatically</button>
```

### React 19

```tsx
import { Link, router } from '@inertiajs/react'

<Link href="/posts">All posts</Link>
<Link href="/posts/42" method="delete" as="button">Delete</Link>

<button onClick={() => router.visit('/posts')}>Visit programmatically</button>
```

### Vue 3.5

```vue
<script setup lang="ts">
import { Link, router } from '@inertiajs/vue3'
</script>

<template>
  <Link href="/posts">All posts</Link>
  <Link href="/posts/42" method="delete" as="button">Delete</Link>
  <button @click="router.visit('/posts')">Visit programmatically</button>
</template>
```

The `router` object also exposes `router.post(url, data)`,
`router.put(url, data)`, `router.patch(url, data)`, `router.delete(url)`, and
`router.reload()` — same shape across all three adapters.

## Forms

Inertia v3 ships a declarative `<Form>` component and the imperative
`useForm` (or `createForm` in Svelte) helper. Both POST back to your Rust
controller; validation errors surface as a structured `errors` prop.

### Svelte 5

```svelte
<!-- frontend/src/pages/Posts/Create.svelte -->
<script lang="ts">
  import { useForm } from '@inertiajs/svelte'

  const form = useForm({
    title: '',
    content: '',
  })

  function submit(e: SubmitEvent) {
    e.preventDefault()
    form.post('/posts')
  }
</script>

<form onsubmit={submit} class="space-y-4">
  <input type="text" bind:value={form.title} placeholder="Title" />
  {#if form.errors.title}
    <p class="text-red-500">{form.errors.title}</p>
  {/if}

  <textarea bind:value={form.content} rows={6}></textarea>

  <button type="submit" disabled={form.processing}>
    {form.processing ? 'Saving…' : 'Create'}
  </button>
</form>
```

### React 19

```tsx
// frontend/src/pages/Posts/Create.tsx
import { useForm } from '@inertiajs/react'

export default function PostCreate() {
  const { data, setData, post, processing, errors } = useForm({
    title: '',
    content: '',
  })

  const submit = (e: React.FormEvent) => {
    e.preventDefault()
    post('/posts')
  }

  return (
    <form onSubmit={submit} className="space-y-4">
      <input
        type="text"
        value={data.title}
        onChange={(e) => setData('title', e.target.value)}
        placeholder="Title"
      />
      {errors.title && <p className="text-red-500">{errors.title}</p>}

      <textarea
        value={data.content}
        onChange={(e) => setData('content', e.target.value)}
        rows={6}
      />

      <button type="submit" disabled={processing}>
        {processing ? 'Saving…' : 'Create'}
      </button>
    </form>
  )
}
```

### Vue 3.5

```vue
<!-- frontend/src/pages/Posts/Create.vue -->
<script setup lang="ts">
import { useForm } from '@inertiajs/vue3'

const form = useForm({
  title: '',
  content: '',
})

function submit() {
  form.post('/posts')
}
</script>

<template>
  <form @submit.prevent="submit" class="space-y-4">
    <input type="text" v-model="form.title" placeholder="Title" />
    <p v-if="form.errors.title" class="text-red-500">{{ form.errors.title }}</p>

    <textarea v-model="form.content" rows="6" />

    <button type="submit" :disabled="form.processing">
      {{ form.processing ? 'Saving…' : 'Create' }}
    </button>
  </form>
</template>
```

## Shared props

Anything you register as a shared prop at boot — typically the current user,
flash messages, and global CSRF token — is available on every page through
`usePage()` (React, Vue) or the reactive `page` store (Svelte). Page props
override shared props on key collision.

### Svelte 5

```svelte
<script lang="ts">
  import { page } from '@inertiajs/svelte'

  let auth = $derived($page.props.auth as { user?: { name: string } })
</script>

{#if auth.user}
  <span>Welcome, {auth.user.name}</span>
{:else}
  <a href="/login">Log in</a>
{/if}
```

### React 19

```tsx
import { usePage } from '@inertiajs/react'

function Header() {
  const { auth } = usePage<{ auth: { user?: { name: string } } }>().props
  return auth.user ? <span>Welcome, {auth.user.name}</span> : <a href="/login">Log in</a>
}
```

### Vue 3.5

```vue
<script setup lang="ts">
import { usePage } from '@inertiajs/vue3'

const page = usePage<{ auth: { user?: { name: string } } }>()
</script>

<template>
  <span v-if="page.props.auth.user">Welcome, {{ page.props.auth.user.name }}</span>
  <a v-else href="/login">Log in</a>
</template>
```

## Layouts

A layout is just a regular component that takes a slot / children / template
content. There's no special Suprnova API — you import a layout and render
your page content inside it.

### Svelte 5

```svelte
<!-- frontend/src/layouts/AppLayout.svelte -->
<script lang="ts">
  import { Link } from '@inertiajs/svelte'
  let { children } = $props()
</script>

<div class="min-h-screen bg-gray-100">
  <nav class="bg-white shadow p-4">
    <Link href="/">Home</Link>
    <Link href="/posts">Posts</Link>
  </nav>
  <main class="max-w-6xl mx-auto py-8">
    {@render children?.()}
  </main>
</div>
```

```svelte
<!-- frontend/src/pages/Posts/Index.svelte -->
<script lang="ts">
  import AppLayout from '../../layouts/AppLayout.svelte'
  import type { PostsIndexProps } from '../../types/inertia-props'

  let { posts }: PostsIndexProps = $props()
</script>

<AppLayout>
  <h1 class="text-2xl font-bold">Posts</h1>
  <ul>
    {#each posts as post (post.id)}
      <li>{post.title}</li>
    {/each}
  </ul>
</AppLayout>
```

### React 19

```tsx
// frontend/src/layouts/AppLayout.tsx
import { Link } from '@inertiajs/react'

export default function AppLayout({ children }: { children: React.ReactNode }) {
  return (
    <div className="min-h-screen bg-gray-100">
      <nav className="bg-white shadow p-4">
        <Link href="/">Home</Link>
        <Link href="/posts">Posts</Link>
      </nav>
      <main className="max-w-6xl mx-auto py-8">{children}</main>
    </div>
  )
}
```

### Vue 3.5

```vue
<!-- frontend/src/layouts/AppLayout.vue -->
<script setup lang="ts">
import { Link } from '@inertiajs/vue3'
</script>

<template>
  <div class="min-h-screen bg-gray-100">
    <nav class="bg-white shadow p-4">
      <Link href="/">Home</Link>
      <Link href="/posts">Posts</Link>
    </nav>
    <main class="max-w-6xl mx-auto py-8">
      <slot />
    </main>
  </div>
</template>
```

## Why Suprnova diverges

Laravel's Inertia integration ships one frontend at a time — you pick React,
Vue, or Svelte at install with a single starter kit per project. Suprnova
keeps the same one-per-project rule (you don't mix), but the CLI scaffolds
to all three idiomatically from the same `inertia_response!` call. The Rust
side never knows which frontend is running; the generator and Vite resolver
pick the right extension on disk.

The other divergence is compile-time component validation. Laravel resolves
the component name at runtime, so a typo in `Inertia::render('Dahsboard')`
becomes a production error. Suprnova's `inertia_response!` macro walks
`frontend/src/pages/` at expansion time and fails `cargo check` with a
"Did you mean 'Dashboard'?" suggestion. The full TypeScript type story
(generated from `#[derive(InertiaProps)]` on the Rust struct) means the
component's props are typed end-to-end too.

## Next

- [Inertia Responses](frontend-inertia-responses.md) — the
  `inertia_response!` macro, partial reloads, deferred props
- [TypeScript Types](frontend-typescript-types.md) — `suprnova generate-types`
  and the typed-props pipeline
- [Frontend Overview](frontend.md) — how the Inertia bridge fits together
- [Inertia CRUD Tutorial](tutorial-inertia-crud.md) — a full Posts resource
  end-to-end
- [Authentication](authentication.md) — wiring the auth pages the starter
  scaffolds for you
