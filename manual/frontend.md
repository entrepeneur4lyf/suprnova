# Frontend Overview

Suprnova bridges Rust handlers to a single-page frontend via
[Inertia.js](https://inertiajs.com/) 3.1.1. You write controllers in Rust
and pages in Svelte, React, or Vue; the framework moves typed props
between them without a separate HTTP API in the middle.

## Three first-class starters

`suprnova new <name>` scaffolds a working project. The `--frontend` flag
picks the SPA layer:

```bash
suprnova new my-app                       # Svelte 5 (default)
suprnova new my-app --frontend svelte     # Svelte 5
suprnova new my-app --frontend react      # React 19
suprnova new my-app --frontend vue        # Vue 3.5
```

All three scaffolds share the same stack:

| Layer | Version |
|---|---|
| Inertia client adapter | `@inertiajs/{svelte,react,vue3}` 3.1.1 |
| Build tool | Vite 8 |
| Styling | Tailwind v4 (`@tailwindcss/vite`) |
| TypeScript | strict mode |

The choice is per-project. There is no "primary" framework on the
server side — `inertia_response!` resolves whichever extension your
chosen scaffold uses (`.svelte`, `.tsx`, `.vue`), and `App::inertia_share`,
partial reloads, and TypeScript prop generation all behave identically
across the three.

## Architecture

```
                       Browser
   +-------------------------------------------------+
   |               SPA (Svelte / React / Vue)        |
   |   +---------------+ +---------------+           |
   |   | Home.svelte   | | Users/Show.tsx|  ...      |
   |   +-------+-------+ +-------+-------+           |
   |           |  typed props from Rust struct       |
   |   +-------v-------------------------------+     |
   |   |        Inertia client adapter         |     |
   +---+------------------+------------------+--+----+
                          |
                          |   HTTP (JSON on XHR, HTML on first load)
                          v
   +-------------------------------------------------+
   |                  Suprnova server                |
   |   +------------------------------------------+  |
   |   |          Controllers / handlers          |  |
   |   |   inertia_response!(&req, "Home",        |  |
   |   |                     HomeProps { ... })   |  |
   |   +------------------------------------------+  |
   +-------------------------------------------------+
```

The first request returns an HTML shell with the initial page object
embedded in the mount node's `data-page` attribute. Subsequent visits
go through `<Link>` / `router.visit`, send `X-Inertia: true`, and get
back a JSON page object — the adapter swaps the component without a
full reload.

## A complete page round-trip

The controller defines its props as a Rust struct, derives
`InertiaProps`, and hands the value to the `inertia_response!` macro:

```rust
use suprnova::{InertiaProps, Request, Response, inertia_response};

#[derive(InertiaProps)]
pub struct HomeProps {
    pub title: String,
    pub message: String,
}

pub async fn index(req: Request) -> Response {
    inertia_response!(&req, "Home", HomeProps {
        title: "Welcome".into(),
        message: "Hello from Suprnova!".into(),
    })
}
```

A few things the macro does for you. First, it validates at compile
time that the page component file actually exists under
`frontend/src/pages/Home.{svelte,tsx,jsx,vue}` — typos surface as a
build error, not a 404 in the browser. Second, it serializes the
`HomeProps` struct, unfolds it into one prop per top-level key so
partial reloads can filter, and resolves any lazy or deferred props
against `&req` before returning. The macro evaluates to a
`Result<HttpResponse, FrameworkError>`, which the `Response` return type
accepts directly.

The matching Svelte page (the default scaffold):

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

For the React and Vue equivalents see [Page Components](frontend-pages.md).

## Generating TypeScript types

Every `#[derive(InertiaProps)]` struct in your `src/` becomes a
TypeScript interface in `frontend/src/types/inertia-props.ts`:

```bash
suprnova generate-types
```

The same command also emits `frontend/src/types/routes.ts` —
type-safe URL + method pairs scraped from your `routes!` macro that
work directly with Inertia v2+ APIs. The full type-mapping table and
route-helper shape live in [TypeScript Types](frontend-typescript-types.md).

## Shared data

Anything that should appear on every page (the authenticated user, the
current locale, app metadata) is registered once at boot and merged into
every Inertia response:

```rust
// In bootstrap.rs
App::inertia_share("appName", "Suprnova");
App::inertia_share("appVersion", env!("CARGO_PKG_VERSION"));

// Async / per-request shared data goes through the trait.
App::register_inertia_shared(Arc::new(AppSharedData));
```

Three flavours, in order of precedence (later wins at the same key):

| API | When the value materializes |
|---|---|
| `App::inertia_share(k, v)` | Sync, set once at boot |
| `App::inertia_share_lazy(k, \|\| async { ... })` | Per response, recomputed |
| `App::inertia_share_once(k, \|\| async { ... })` | Per response, then client-cached |
| `App::register_inertia_shared(Arc::new(impl))` | Per request, sees `&req` |

Per-page props attached on the response builder always overwrite shared
data at the same key.

## Partial reloads and lazy props

The same `InertiaResponse` builder exposes Inertia v3's full prop
toolkit — eager, lazy, optional, deferred, merge, once — and Suprnova
honors the v3 partial-reload headers (`X-Inertia-Partial-Data`,
`X-Inertia-Partial-Except`, `X-Inertia-Reset`,
`X-Inertia-Except-Once-Props`) automatically. The example below
attaches three props with different evaluation rules:

```rust
use suprnova::{InertiaResponse, FrameworkError, Request, Response};

pub async fn dashboard(req: Request) -> Response {
    let resp = InertiaResponse::new("Dashboard")
        .with("title", "Dashboard")
        .lazy("recent_orders", || async {
            Ok::<_, FrameworkError>(load_recent_orders().await?)
        })
        .defer("notifications", || async {
            Ok::<_, FrameworkError>(load_notifications().await?)
        })
        .resolve(&req)
        .await?;
    Ok(resp)
}
```

`inertia_response!` covers the eager-props case; everything past that
goes through the builder. The full surface — `optional`, `merge`,
`once`, `scroll`, `flash`, `paginate`, SSR, version mismatch, history
encryption — is documented in
[Inertia Responses](frontend-inertia-responses.md).

## Bootstrap

A scaffolded app installs the two protocol-critical middlewares in one
call inside `bootstrap.rs`:

```rust
use suprnova::{Inertia, InertiaConfig};

Inertia::install(&InertiaConfig::new().version(env!("CARGO_PKG_VERSION")));
```

That registers `InertiaVersionMiddleware` (emits 409 + `X-Inertia-Location`
on asset-version mismatch so stale clients reload) and `Inertia303Middleware`
(rewrites 302 → 303 on non-GET Inertia visits so the follow-up is
unambiguously a GET). Both used to be opt-in; `Inertia::install` makes
them the default.

## Development vs production

In development, the Vite dev server runs alongside the backend and
serves HMR-enabled assets:

```bash
suprnova serve
```

This boots the Rust server and `vite` together. The HTML shell loads
modules from `http://localhost:5173`.

For production, build the frontend once and point the backend at the
hashed manifest under `public/assets/`:

```bash
cd frontend && npm run build
SUPRNOVA_ENV=production suprnova serve --backend-only
```

Suprnova reads `public/assets/.vite/manifest.json` to resolve hashed
entry points plus any transitive imports for `modulepreload`. SSR is
optional — opt in by pointing `InertiaConfig::ssr(...)` at a running
`@inertiajs/{vue3,react,svelte}/server` worker.

### Why Suprnova diverges

Three intentional departures from how a typical Inertia setup looks
elsewhere:

- **Compile-time component validation.** The `inertia_response!` macro
  walks `frontend/src/pages/` at build time and refuses to expand if
  the component file is missing, suggesting the closest match. You
  cannot ship a controller that points at a deleted page.
- **Typed props as the source of truth.** Page props are Rust structs
  with `#[derive(InertiaProps)]`. `suprnova generate-types` reads them
  and writes TypeScript interfaces — the frontend types are derived
  from the backend, not maintained in parallel.
- **Svelte as the default.** Inertia's documentation reaches for Vue and
  React first; the Suprnova scaffolder defaults to Svelte 5 (runes-on).
  React 19 and Vue 3.5 are first-class, not afterthoughts — same
  protocol, same prop pipeline, same generator output.

## Next

- [Page Components](frontend-pages.md)
- [Inertia Responses](frontend-inertia-responses.md)
- [TypeScript Types](frontend-typescript-types.md)
- [Routing](routing.md)
- [Controllers](controllers.md)
