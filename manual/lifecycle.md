# Request Lifecycle

What actually happens between the TCP packet hitting the socket and
your handler returning a `Response`? Six files. Trace them once and
the framework's shape clicks into place.

## The path

```
                                        ┌─────────────────────┐
                                        │  bind socket        │
                                        │  (server.rs)        │
                                        └──────────┬──────────┘
                                                   │
                                                   ▼
                                        ┌─────────────────────┐
                                        │  hyper accepts      │
                                        │  HTTP/1.1, h2, WS   │
                                        └──────────┬──────────┘
                                                   │
                                                   ▼
                                        ┌─────────────────────┐
                                        │  handle_request     │
                                        │  - WS upgrade?      │
                                        │  - health endpoint? │
                                        │  - task-locals      │
                                        └──────────┬──────────┘
                                                   │
                                                   ▼
                                        ┌─────────────────────┐
                                        │  handle_request_    │
                                        │  inner              │
                                        │  - match_route      │
                                        │  - build chain      │
                                        └──────────┬──────────┘
                                                   │
                                                   ▼
                                        ┌─────────────────────┐
                                        │  execute_chain      │
                                        │  _safely            │
                                        │  - panic boundary   │
                                        │  - run middleware   │
                                        │  - run handler      │
                                        └──────────┬──────────┘
                                                   │
                                                   ▼
                                        ┌─────────────────────┐
                                        │  HttpResponse       │
                                        │  on the wire        │
                                        └─────────────────────┘
```

## 1. Boot — `app.rs`

A scaffolded app's `main()` builds an `Application` fluently and runs
it:

```rust
Application::new()
    .config(my_app::config::register)
    .bootstrap(my_app::bootstrap::bootstrap)
    .routes(my_app::routes::register)
    .migrations::<my_app::migrations::Migrator>()
    .run()
    .await
```

`Application::run()` parses the binary's CLI (clap):

- `serve` — start the HTTP server
- `web:run` — alias for serve
- `migrate` / `migrate:rollback` / `migrate:status` / `migrate:fresh`
- `db:sync` / `db:seed`
- `schedule:run` / `schedule:work` / `schedule:list`
- `workflow:work`
- `queue:work`

For `serve`, it then:

1. Loads `.env` via `Config::init(".")` and detects `Environment`
2. Drains the `#[policy]` inventory into the authorization system
3. Calls your `config_fn` (typed config registration)
4. Runs migrations
5. Calls your `bootstrap_fn` (service registration, observers, listeners)
6. Builds the `Router` from `routes_fn`
7. Hands the router to `Server::from_config(...)`
8. Calls `server.run()`

The same boot path is used by workers (`queue:work`, `workflow:work`,
`schedule:run`) so they see the same configured services and bound
container values.

## 2. Server boot — `server.rs`

`Server::from_config` does two things that matter for safety:

- Runs `App::init()` + `App::boot_services()` — initialises the
  container's task-local layer and resolves boot-time dependencies
- **Fails closed** when `APP_KEY` is required (any non-development
  environment) but missing/malformed — returns `Err`, and `app.rs`
  prints a remediation message and exits non-zero instead of panicking

`server.run()` then:

1. Boots telemetry (`tracing` subscriber, log format)
2. Loads encryption keys (`APP_KEY` + `APP_KEY_PREVIOUS`)
3. Boots the runtime drivers **in this exact order**: Cache → Queue →
   RateLimit → Mail. Non-server subcommands also call
   `bootstrap_runtime_drivers` so workers see the same drivers
4. Binds the TCP socket
5. Serves over hyper with `.with_upgrades()` (so WebSocket upgrades work)

The driver boot order is intentional — Queue may depend on Cache
(for unique-job locks), RateLimit may use Cache, Mail may dispatch
via Queue.

## 3. Request entry — `handle_request`

Every request lands in `handle_request(router, registry, req)`.
**This is also the in-process request surface integration tests
drive without opening a socket.** It's re-exported as
`suprnova::handle_request`.

```rust
pub async fn handle_request(
    router: Arc<Router>,
    registry: Arc<MiddlewareRegistry>,
    req: Request,
) -> HttpResponse;
```

Inside, it:

1. Checks for a WebSocket upgrade via `router.match_ws(...)` — if it
   matches a `ws!()` route, hands off to the WS handler
2. Special-cases the built-in health endpoint at `GET /_suprnova/health`
3. Installs per-request task-locals (flash bag, SSR-disable flag)
4. Dispatches into `handle_request_inner`

## 4. Routing + chain assembly — `handle_request_inner`

This is where the middleware chain composes. The router yields a
`(pattern, handler, params)` triple, and the `MiddlewareChain` is
assembled in this fixed order:

```
[0] RequestIdMiddleware (always outermost)
[1] global middleware in registration order
[2] route middleware (keyed by (method, matched pattern))
[3] handler
```

Three things to notice:

- **Pattern, not path.** Route middleware is keyed by the matched
  pattern (`"/posts/{id}"`), not the raw path (`/posts/42`). Group
  middleware on parameterised routes actually fires.
- **No match still runs the chain.** If the router doesn't match any
  route, the chain (RequestId + globals) still runs and terminates in
  a registered fallback or a static 404. CORS preflight (OPTIONS rarely
  matches a route), logging, and request-id all reach unrouted traffic.
- **Group middleware is flattened, not stacked.** Group middleware is
  copied into each grouped route's middleware list at register time —
  it isn't a separate runtime layer. Introspection can't tell group
  from route middleware apart.

## 5. Panic boundary — `execute_chain_safely`

The chain runs inside `AssertUnwindSafe(...).catch_unwind()`. **A panic
in any middleware or the handler is caught**, logged with method+path,
and converted through the same `FrameworkError → HttpResponse` path
as a returned 5xx:

- Sanitised body: `{"message": "Internal Server Error"}`
- `request_id` injected so you can correlate with the log
- `ErrorOccurred` event dispatched so listeners (Sentry, your alert
  pipeline) see the failure
- The panic payload **never leaks into the response body**

This is a safety net, not a contract. Public APIs in your code should
return `Result`, not rely on `catch_unwind`. The boundary exists to
keep a buggy handler from killing the worker thread or leaking a stack
trace to the client — it isn't licence to `.unwrap()` everywhere.

## 6. Chain composition — `middleware/chain.rs`

`MiddlewareChain::execute` nests the handler as the innermost `Next`,
then wraps each middleware last-to-first (`.rev()`), so **the
first-added middleware runs first** (outside-in). An empty chain calls
the handler directly:

```
register order:   [Auth, CSRF, Throttle, handler]
runtime order:    Auth → CSRF → Throttle → handler → (back out)
```

If middleware short-circuits (returns `Err(response)`), the chain
unwinds immediately and the response goes back out through the
already-executed middleware in reverse.

## The `Response` contract

`http::Response` is **`Result<HttpResponse, HttpResponse>`** — both
arms carry an `HttpResponse`. Handlers and `Middleware::handle` return
`Response`:

- `Ok(resp)` is success
- `Err(resp)` short-circuits — for example, a 401 straight from auth
  middleware. The runtime collapses both with
  `result.unwrap_or_else(|e| e)`, so an `Err` is a response, not a
  crash.
- `?` propagates any error that converts to `HttpResponse`. Every
  `FrameworkError`, `AppError`, `ValidationErrors`, and your own
  `HttpError` impls do — so handler bodies read top-to-bottom and
  bubble failures to the converter.

The error converter (`From<FrameworkError> for HttpResponse`)
sanitises 5xx bodies and never leaks detail to the wire. The detail
stays in the structured log.

See [Error Handling](errors.md) and [Error Model](error-model.md) for
the full picture.

## Per-request state

Two layers of per-request state, both task-local:

- **Flash bag** — `req.flash()` returns the session flash; values stored
  here survive one redirect and then disappear
- **SSR-disable flag** — Inertia uses this to short-circuit
  server-side rendering in test contexts

Both are installed by `handle_request` before the chain runs and
torn down when the response leaves. Custom per-request state goes
through the `Context` system — see [Context](context.md).

## Workers reuse the same lifecycle

Background workers (`queue:work`, `workflow:work`, `schedule:run`) go
through:

1. The same boot path (`Config::init`, `bootstrap_runtime_drivers`,
   your `bootstrap()` function)
2. Their own loop that pulls work and runs handlers with the **same
   panic boundary** (`execute_chain_safely` equivalent for each worker
   type)
3. Graceful shutdown on `SIGTERM` / `SIGINT` — in-flight work finishes,
   no new work starts

This means an observer registered in `bootstrap()` fires for inserts
from a queue worker exactly as it would for inserts from an HTTP
handler.

## Production safety guarantees

A short list of invariants the lifecycle establishes:

- **`APP_KEY` is required in non-development environments.** Boot fails
  closed, exits non-zero, no encrypted-data corruption.
- **Panics in handler or middleware never reach the client.** The
  panic-boundary returns a sanitised 500 and dispatches `ErrorOccurred`.
- **5xx bodies are always sanitised.** Detail goes to the log, not the
  wire.
- **Poisoned locks never abort the process.** Two sanctioned patterns:
  per-request paths map poison to `FrameworkError::lock_poisoned` (and
  the request gets a 503); hot-path registries that must stay up
  recover in place with `.unwrap_or_else(|e| e.into_inner())`. See
  [Lock Policy](lock-policy.md).
- **Driver backend failures are an explicit fail-open or fail-closed
  choice.** Rate-limit, cache, session each pick a policy at the call
  site — `BackendErrorPolicy::FailClosed` returns 503; `FailOpen`
  lets the request through. There is no implicit default. See
  [Rate Limiting](rate-limiting.md).
- **WebSocket upgrades go through the same router.** The same
  `match_ws` lookup uses the same `(method, pattern)` indexing as
  HTTP routes; you can apply per-route WS middleware exactly like
  HTTP middleware.

## What this means for your code

A few takeaways for day-to-day handler writing:

- **Return `Response`, propagate with `?`.** Don't `match err` unless
  you need the bare `HttpResponse`.
- **Implement `HttpError` on your domain error types.** They'll
  convert automatically. See [Error Handling](errors.md).
- **Don't rely on the panic boundary.** It catches genuine bugs and
  prevents process crashes; library code should still return `Result`.
- **Middleware order matters and is fixed in three layers** —
  request-id outermost, globals next, route middleware innermost
  before the handler.
- **Workers and handlers share bootstrap.** Anything you register at
  boot is visible to both.

## Where each step lives

| Step | File |
|---|---|
| Boot | `framework/src/app.rs` |
| Server lifecycle | `framework/src/server.rs` |
| `handle_request` (entry) | `framework/src/server.rs` (re-exported as `suprnova::handle_request`) |
| `handle_request_inner` (routing + chain) | `framework/src/server.rs` |
| `execute_chain_safely` (panic boundary) | `framework/src/server.rs` |
| `MiddlewareChain::execute` (composition) | `framework/src/middleware/chain.rs` |
| Router matching | `framework/src/routing/router.rs` |

You shouldn't need to read these to use the framework, but if a bug
surprises you, the trail is short.

## Next

- [Service Container](container.md) — how `App::*` resolves services
- [Application Bootstrap](bootstrap.md) — what `bootstrap.rs` does
- [Middleware](middleware.md) — writing your own middleware
- [Error Model](error-model.md) — `FrameworkError`, `HttpError`,
  panic recovery in detail
- [Routing](routing.md) — what `routes!` actually expands into
