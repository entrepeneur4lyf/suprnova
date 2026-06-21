# Inertia Responses

Inertia responses are how a Suprnova handler ships state to a Svelte / React /
Vue page component. Every handler that renders an Inertia page returns one,
built either through the [`inertia_response!`](#the-inertia_response-macro)
macro (for typed, compile-time-checked eager props) or the
[`InertiaResponse`](#the-inertiaresponse-builder) builder (for everything
else — lazy props, deferred props, merge, once, scroll, flash). This
chapter covers the response surface end-to-end: the macro, the builder, the
v3 protocol features (partial reloads, history encryption, version
detection), shared data via `App::inertia_share*`, and the flash bag carried
across redirects.

If you haven't picked a frontend yet, [Frontend Overview](frontend.md) and
[Page Components](frontend-pages.md) come first; this chapter assumes the
SPA bridge is wired and focuses on what your handler returns.

## The `inertia_response!` macro

The macro is the shortest path from a handler to a typed eager page. It
takes the current request, a component name, and a props expression:

```rust
use suprnova::{Request, Response, inertia_response, InertiaProps};

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

Three things to know:

- **The leading `&req` is required.** The macro reads `X-Inertia` headers,
  the URL, and the partial-reload filtering headers off the request, so it
  needs the request value (or a reference). Without it, partial reloads
  would silently break.
- **Component existence is checked at compile time.** The macro looks for
  `frontend/src/pages/<Component>.{svelte,tsx,jsx,vue}`; if no file
  matches, the build fails with a "did you mean…?" suggestion sourced from
  the actual filenames on disk. Nested paths work the same way —
  `inertia_response!(&req, "Admin/Dashboard", …)` resolves
  `frontend/src/pages/Admin/Dashboard.svelte` (or your frontend's
  extension).
- **The macro expands to an `await`ed `Result`.** Your handler must
  return [`Response`](error-model.md) (which is
  `Result<HttpResponse, HttpResponse>`) or another type that absorbs
  `FrameworkError` through `?` / `From`. Failures during prop
  serialization or response building are returned as `Err`, not panics.

### JSON-style props

For prototyping and tiny pages you can skip the typed struct:

```rust
inertia_response!(&req, "Dashboard", {
    "user": { "name": "John" },
    "stats": { "visits": 1234 }
})
```

The macro still validates the component file. The trade-off is that you
lose the typed-prop chain — no `#[derive(InertiaProps)]`, no automatic
TypeScript generation, no compile-time check that the frontend's
expected shape matches.

### Optional config override

The macro accepts an optional trailing `InertiaConfig` for per-response
overrides (different SSR settings, a custom default title for one page):

```rust
let cfg = InertiaConfig::new().default_title("Reports");
inertia_response!(&req, "Reports/Index", props, cfg)
```

Most apps register a single config at boot via [`Inertia::install`](#bootstrap-inertia-install)
and never touch this argument.

## `#[derive(InertiaProps)]`

`InertiaProps` emits a `Serialize` impl whose key names match your field
names. It exists so the typed-props path stays terse and so the
TypeScript generator (`suprnova generate-types`) has a marker to find:

```rust
use suprnova::InertiaProps;

#[derive(InertiaProps)]
pub struct UserProps {
    pub name: String,
    pub email: String,
    pub role: String,
    pub is_active: bool,
}
```

Nested types compose normally — fields can be `Vec<T>`, `Option<T>`,
nested structs, anything `Serialize`-able. The nested types themselves
don't have to derive `InertiaProps`; they just need `Serialize`. Use
`#[derive(InertiaProps)]` on the *top-level* props struct and you get
the automatic TypeScript surface (see [TypeScript Types](frontend-typescript-types.md))
for the whole tree.

## The `InertiaResponse` builder

The macro covers eager typed props. Anything else — lazy, optional, deferred,
mergeable, cached-on-client, flash, history-encryption overrides — uses
the builder directly:

```rust
use suprnova::{InertiaResponse, Request, Response, FrameworkError, HttpResponse};

pub async fn show(req: Request) -> Response {
    let resp = InertiaResponse::new("Posts/Show")
        .with("title", "Welcome")
        .with("post", load_post(42).await?)
        // Lazy: closure runs only when the prop will actually be sent
        // (initial visit, or partial reload that requests this key).
        .lazy("recent_activity", || async {
            Ok::<_, FrameworkError>(load_activity().await?)
        })
        // Optional: never sent on initial visits; the client must
        // explicitly ask for the key via X-Inertia-Partial-Data.
        .optional("permissions", || async {
            Ok::<_, FrameworkError>(load_permissions().await?)
        })
        // Defer: skipped on the initial render; the client issues a
        // follow-up XHR and the closure runs then.
        .defer("notifications", || async {
            Ok::<_, FrameworkError>(load_notifications().await?)
        })
        // Merge: append-into-existing on partial reloads ("load more").
        .merge("rows", next_page().await?)
        // Once: cached client-side across navigations; resolver skipped
        // on subsequent visits unless server forces refresh.
        .once("plans", || async {
            Ok::<_, FrameworkError>(load_plan_catalog().await?)
        })
        // Flash: one-shot toast; appears under `page.flash`, not `props`.
        .flash("toast", serde_json::json!({"type":"info","msg":"Saved"}))
        .resolve(&req)
        .await
        .map_err(HttpResponse::from)?;
    Ok(resp)
}
```

| Method | Purpose | Maps to Laravel |
|---|---|---|
| `.with(k, v)` | Eager prop, honours partial-reload filtering | typed prop |
| `.always(k, v)` | Eager prop, ignores partial-reload filters | `Inertia::always(…)` |
| `.lazy(k, ‖)` | Resolver runs only when prop will be sent | `fn () => …` closure |
| `.optional(k, ‖)` | Never on initial visit; must be requested explicitly | `Inertia::optional(…)` |
| `.defer(k, ‖)` / `.defer_with(...)` | Initial-visit-skipped; follow-up XHR triggers resolution | `Inertia::defer(…)` |
| `.merge` / `.merge_prepend` / `.deep_merge` / `.merge_with` | Combine with existing client state on partial reloads | `Inertia::merge` / `deepMerge` |
| `.once(k, ‖)` / `.once_with(…)` | Client caches across navigations | `Inertia::once(…)` |
| `.scroll` / `.scroll_with` / `.paginate` (via `Inertia::paginate`) | Infinite-scroll pagination | `Inertia::scroll(…)` |
| `.flash(k, v)` | One-shot value under `page.flash` (not `props`) | `session()->flash(…)` |
| `.title(…)` | Default `<title>` for the HTML shell | `Inertia::render(…)->title(…)` |
| `.encrypt_history(bool)` | Per-response history encryption | `Inertia::encryptHistory(…)` |
| `.clear_history()` | Force history key rotation | `Inertia::clearHistory()` |
| `.preserve_fragment(bool)` | Keep `#fragment` after Inertia visit | `Inertia::preserveFragment()` |

Eager builder methods have `try_*` siblings (`try_with`, `try_always`,
`try_merge_with`, `try_scroll`, `try_flash`) that return
`Result<Self, FrameworkError>` when a value's `Serialize` impl might
fail at runtime — the infallible methods convert the panic into a 500
via [the panic boundary](error-model.md), so reach for `try_*` when
you'd rather handle the failure explicitly.

### Merge strategies and infinite scroll

`.merge` (append), `.merge_prepend`, and `.deep_merge` cover the common
"load more" cases. To diff-merge — update rows the client already holds
instead of duplicating them — reach for `.merge_with` with an explicit
`MergeStrategy` carrying a `match_on` key:

```rust
use suprnova::{InertiaResponse, MergeStrategy};

InertiaResponse::new("Feed/Index")
    .merge_with(
        "posts",
        next_page,                                     // the new page slice
        MergeStrategy::Append { match_on: Some("id".into()) },
    )
```

`match_on` names the field the client dedupes on (emitted to the page
object as `matchPropsOn`), so a refetch that overlaps the current window
replaces matching rows in place rather than appending copies. `Prepend`
and `Deep` take the same `match_on`.

Infinite scroll is the same machinery with pagination metadata attached.
`.scroll` / `.scroll_with` — or `.paginate`, which adapts a
`LengthAwarePaginator` or `CursorPaginator` directly — emit `scrollProps`
next to the data, and the client's `<InfiniteScroll>` component drives the
next/previous fetches:

```rust
// `posts` is a CursorPaginator from the query builder.
InertiaResponse::new("Feed/Index").paginate("posts", posts)
```

The framework reads the merge direction from the
`X-Inertia-Infinite-Scroll-Merge-Intent` request header the client sends
(`append` when scrolling down, `prepend` when scrolling up). On a fresh
visit — no intent header — `scrollProps["posts"].reset` is `true`, so the
client clears its accumulator before rendering the first window.

## Partial reloads

The Inertia 3 client can request a subset of a page's props (or a
superset by including an Optional or Defer key). The protocol uses
three request headers:

| Header | Meaning |
|---|---|
| `X-Inertia-Partial-Component` | The component being partial-reloaded — must match the response's component for filtering to apply. |
| `X-Inertia-Partial-Data` | Whitelist: comma-separated prop keys to include. |
| `X-Inertia-Partial-Except` | Blacklist: comma-separated prop keys to exclude. Wins over `Partial-Data` on key collision. |

Filtering rules:

- `Eager`, `Lazy`, `Merge`, `Once`, `Scroll` props follow whitelist /
  blacklist semantics.
- `Always` props are sent regardless.
- `Optional` and `Defer` props are never on a standard visit and only
  appear on a matching partial reload that explicitly lists the key.

The handler doesn't have to do anything special — register every prop
through the builder, and the framework consults the headers when
serializing the page object.

## Shared data via `App::inertia_share*`

Some props are the same on every Inertia page — auth state, the CSRF
token, the current locale, app-wide flags. Register them once at
bootstrap and they merge into every response:

```rust
use suprnova::App;
use std::sync::Arc;

pub fn register() {
    // Sync, materialized once at boot.
    App::inertia_share("appName", "Suprnova");
    App::inertia_share("appVersion", env!("CARGO_PKG_VERSION"));

    // Async, resolved per response (skipped by partial reloads that
    // exclude the key).
    App::inertia_share_lazy("locale", || async {
        Ok::<_, suprnova::FrameworkError>(detect_locale().await)
    });

    // Cached on the client across navigations — `share_once` runs on
    // the first page that needs it, then the client skips re-resolution
    // via `X-Inertia-Except-Once-Props` until the cache key changes.
    App::inertia_share_once("plans", || async {
        Ok::<_, suprnova::FrameworkError>(load_plan_catalog().await?)
    });
}
```

For per-request shared data (the authenticated user, request-scoped
flags), implement [`InertiaSharedData`](#per-request-shared-data) and
register the singleton — the framework calls `share(&req)` on every
Inertia response and merges the result.

### Precedence on key collision

When the same key appears in more than one layer, later writes win:

1. Static registry (`App::inertia_share` / `App::inertia_share_lazy`)
2. Per-request trait provider (`InertiaSharedData::share`)
3. Per-response builder methods (`.with`, `.lazy`, etc.)

This lets a handler override a globally-shared default for one page
without having to unregister anything.

### Per-request shared data

The trait runs once per Inertia response with access to the request.
Implementations need `async_trait` (re-exported as `suprnova::__async_trait`)
and `IndexMap` (re-exported as `suprnova::indexmap`):

```rust
use suprnova::{
    App, Auth, FrameworkError, InertiaRequestExt, InertiaSharedData, Prop,
    indexmap::IndexMap,
};
use std::sync::Arc;

pub struct AuthShare;

#[suprnova::__async_trait]
impl InertiaSharedData for AuthShare {
    async fn share(
        &self,
        _req: &dyn InertiaRequestExt,
    ) -> Result<IndexMap<String, Prop>, FrameworkError> {
        let mut out = IndexMap::new();
        if let Some(user) = Auth::user().await? {
            out.insert(
                "auth".into(),
                Prop::Eager(serde_json::json!({
                    "id": user.get_auth_identifier(),
                })),
            );
        }
        Ok(out)
    }
}

// In bootstrap:
App::register_inertia_shared(Arc::new(AuthShare));
```

## Flash and redirects

Flash data is one-shot state that should appear on the next render and
disappear after — toast messages, "just created" IDs, validation summaries.
Suprnova surfaces it under `page.flash` on every Inertia response. There
are three writers:

```rust
// 1. Push into the current request's flash bag.
App::flash("toast", "Saved");

// 2. Attach to a specific response (same effect on this response only).
InertiaResponse::new("Posts/Show").flash("toast", "Saved")

// 3. Carry across a redirect via the Redirect facade.
use suprnova::Redirect;

Redirect::to("/posts").with("toast", "Created")
```

The `Redirect::with(key, value)` form is the cross-handler path: the
value lands in the session under `_flash.new.*`, the next request's
[`SessionMiddleware`](csrf.md) ages it into `_flash.old.*`, and the
destination's `InertiaResponse` surfaces it under `page.flash`.

Same-request flash (the task-local bag) wins over inherited session
flash on key collision, so a destination handler can override an
inbound value just by re-flashing the key.

Internal session keys (anything prefixed `_`) are filtered out of
`page.flash` — `_old_input` for form repopulation and `_inertia.*`
protocol flags don't leak to the client.

### Redirect helpers

`Redirect` is the full Laravel surface:

```rust
Redirect::to("/dashboard")                       // 302 to a path
Redirect::route("posts.show").with("id", "42")   // named route, route params
Redirect::back("/")                              // session-recorded previous URL
Redirect::refresh()                              // same URL, fresh GET
Redirect::guest(&req, "/login")                  // stashes intended URL
Redirect::intended("/dashboard")                 // pops the stashed URL
Redirect::signed_route("downloads.show", &[("id","42")])?  // signed URL
Redirect::to("/posts/42").preserve_fragment()    // keep #frag across visit
```

All `Redirect` variants accept `.with(k, v)`, `.with_input(map)`,
`.with_errors(map)`, `.with_errors_bag(name, map)`, `.cookie(c)`,
`.header(k, v)`, `.permanent()`, `.status(303)`, etc. The full chain
mirrors Laravel's `RedirectResponse`.

For non-GET Inertia visits, the framework auto-converts the response to
`303 See Other` when [`Inertia303Middleware`](#bootstrap-inertia-install)
is installed, so the browser issues a clean follow-up GET instead of
re-submitting the original PUT/PATCH/DELETE to the redirect target.

## Version detection

Inertia versions the asset manifest so a long-lived client doesn't try
to mount a page from yesterday's bundle against today's server. When
the client's `X-Inertia-Version` header doesn't match the server's
configured version, [`InertiaVersionMiddleware`](#bootstrap-inertia-install)
responds with `409 Conflict` and an `X-Inertia-Location` header naming
the new URL — the Inertia client picks that up and does a full page
reload, picking up the new bundle.

You set the version through `InertiaConfig`:

```rust
use suprnova::InertiaConfig;

// Static — most apps. Bake in a build-time identifier.
let cfg = InertiaConfig::new().version(env!("CARGO_PKG_VERSION"));

// Dynamic — read a manifest hash, container deployment ID, anything.
// The closure runs on every version check; cache inside if it isn't cheap.
let cfg = InertiaConfig::new().version_with(|| current_manifest_hash());
```

For async or fallible version resolution (e.g. read a manifest hash
from S3), do the read once at boot and pass the cached `String` to
`.version(...)`.

## Bootstrap: `Inertia::install`

Most apps install the two protocol middlewares in one call:

```rust
use suprnova::{Inertia, InertiaConfig};

pub fn register() {
    let cfg = InertiaConfig::new()
        .version(env!("CARGO_PKG_VERSION"))
        .default_title("My App");

    Inertia::install(&cfg);
    // …other shared data, routes, etc.
}
```

`Inertia::install` registers, in order:

1. `InertiaVersionMiddleware` — emits the `409` + `X-Inertia-Location`
   when client and server disagree on the asset version.
2. `Inertia303Middleware` — upgrades `302` to `303` on non-GET Inertia
   redirects.

Skip the call only if you genuinely don't want one of these middlewares
(rare; both close real failure modes — silent stale-bundle and
form-replay-on-redirect).

## SSR

Suprnova talks to an out-of-process SSR worker — typically the
`@inertiajs/{svelte,react,vue}/server` `createServer()` bundle run
under Node / Bun / Deno — over HTTP loopback. Enable it on the
config:

```rust
InertiaConfig::new()
    .ssr("http://127.0.0.1:13714")  // worker URL
    .ssr_timeout(std::time::Duration::from_millis(500))
    .ssr_exclude("/admin/**")
    .ssr_max_response_bytes(8 * 1024 * 1024)
```

SSR is off by default. When enabled, the framework posts the page
object to `<url>/render` and inlines `{ head, body }` in the HTML
shell. On worker error or timeout the response falls back to CSR
(an empty `<div id="app">` the client hydrates) and the
`on_ssr_error(...)` hook fires; flip `ssr_throw_on_error(true)` in CI
to make those failures hard 500s instead.

Boot the worker separately — `suprnova ssr:start` is the standard
runner once your project ships an SSR entry.

## Configuration

Inertia behaviour is configured programmatically via `InertiaConfig`.
The one env var the framework reads directly is `SUPRNOVA_FRONTEND`
(`svelte` / `react` / `vue`), which selects the default entry-point
filename and page-component extensions. Everything else is
builder-shaped:

```rust
use suprnova::{InertiaConfig, Frontend};

let cfg = InertiaConfig::new()
    .frontend(Frontend::Svelte)              // overrides SUPRNOVA_FRONTEND
    .vite_dev_server("http://localhost:5765")
    .entry_point("src/main.ts")
    .version(env!("CARGO_PKG_VERSION"))
    .default_title("My App")
    .manifest_path("public/assets/.vite/manifest.json")
    .assets_base_url("/assets")
    .max_concurrent_resolvers(16)            // cap lazy-prop fan-out
    .production();                           // false → loads from Vite dev server
```

Frontend-specific defaults:

| Frontend | Default entry point | Page extensions |
|---|---|---|
| Svelte (default) | `src/main.ts` | `.svelte` |
| React | `src/main.tsx` | `.tsx`, `.jsx` |
| Vue | `src/main.ts` | `.vue` |

The Vite manifest at `manifest_path` is loaded lazily on first request
and cached for the process lifetime. When it's missing, production
asset tags fall back to a hardcoded legacy path and a `tracing::warn!`
fires so the gap surfaces in logs.

### Why Suprnova diverges

Laravel's Inertia adapter has a single global "shared data"
registry plus a per-request `Inertia::share($k, $v)` call. PHP's
request-per-process model makes this safe: a fresh process per request
means no leakage between concurrent visitors.

Rust's process model is the opposite — one process serves many
concurrent requests across many threads. So the registry lives on
the [container](container.md) (task-local → thread-local → global),
not in process-global statics. `App::inertia_share*` writes to the
active container's `InertiaRegistry`, which gives tests using
`TestContainer::fake()` clean isolation without having to unregister
anything. Same surface as Laravel; different machinery underneath
because the runtime is different.

Two other Rust-shaped choices worth flagging:

- **Lazy-prop resolvers run concurrently**, capped by
  `max_concurrent_resolvers` (default 16). A page with twelve lazy
  props issues twelve parallel queries inside one Tokio task — that's
  what we built the framework on top of Tokio for. Tune the cap if a
  page has many lazy props each hitting an external service.
- **The compile-time component check** isn't a Laravel feature at all,
  because PHP can't see your frontend files at compile time. Suprnova
  does, so a typo in `inertia_response!("Dashbaord", …)` fails the
  build with a "did you mean Dashboard?" suggestion instead of
  surfacing as a runtime "component not found" later.

## Next

- [Page Components](frontend-pages.md) — how the frontend resolves a
  component name to a Svelte / React / Vue module
- [TypeScript Types](frontend-typescript-types.md) — `suprnova generate-types`
  emits TS definitions from your `#[derive(InertiaProps)]` structs
- [Data Objects](data.md) — `#[derive(Data)]` for DTOs with per-field
  include/allowlist gating that composes with partial reloads
- [Error Model](error-model.md) — how `Response`, the panic boundary,
  and `FrameworkError` thread through Inertia responses
- [Container](container.md) — the lookup model behind
  `App::inertia_share*` and `InertiaSharedData`
