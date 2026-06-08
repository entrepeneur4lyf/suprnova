# CORS

`CorsMiddleware` answers preflight `OPTIONS` requests and decorates
ordinary cross-origin responses with `Access-Control-Allow-*` headers.
You install it once in `bootstrap()` when a browser on a different
origin calls your API — public APIs, an SPA hosted on a different
domain, a mobile webview, or a separately-hosted docs site. Same-origin
apps (Inertia served from the same host as the backend, the Suprnova
default) don't need CORS at all. The middleware mirrors Laravel's
`HandleCors` and `config/cors.php`, but as a typed builder on
`CorsConfig`.

## Install it globally

```rust,ignore
use std::time::Duration;
use suprnova::{global_middleware, CorsConfig, CorsMiddleware};

pub fn register() {
    global_middleware!(CorsMiddleware::new(
        CorsConfig::allow_origins(["https://app.example"])
            .allow_credentials(true)
            .max_age(Duration::from_secs(600)),
    ));
}
```

A preflight is an `OPTIONS` request with an `Access-Control-Request-Method`
header. The router has no `OPTIONS` routes, so a preflight never *matches*
a route — but Suprnova's server runs the global middleware chain on
unmatched requests (terminating in a 404), so a globally-installed
`CorsMiddleware` sees the preflight and short-circuits it with `204`
before the 404 is ever produced. **This is why CORS must be installed
globally, not per-route.**

## Choosing an origin policy

There is intentionally no `Default` for `CorsConfig`. A reflexively
permissive policy is a security footgun, so you must pick:

| Builder | Behavior |
| --- | --- |
| `CorsConfig::allow_origins([...])` | Fixed allow-list. Origin is echoed back only when it exactly matches one entry. |
| `CorsConfig::any_origin()` | Wildcard `*`. With credentials enabled, the middleware echoes the specific request origin instead of `*` (the `*` + credentials combination is invalid per the Fetch spec). |
| `.allow_origin_patterns([...])` | Regex patterns added on top of the literal list. Useful for dynamic subdomains. |

```rust,ignore
CorsConfig::allow_origins(["https://app.example"])
    .allow_origin_patterns([r"^https://[a-z0-9-]+\.staging\.example$"])
```

Patterns are anchored automatically — `^` and `$` are prepended / appended
if missing, so a partial match against a redirect URL like
`https://evil.com/?u=https://app.example` cannot leak through.

Invalid regex panics at config time (boot), not at request time — surface
the config bug loud rather than fail-open silently.

`allowed_origins_patterns` (Laravel-named alias) is also available.

## Scoping which paths get CORS

Laravel's `cors.php` config has a `paths` array (`['api/*',
'sanctum/csrf-cookie']`) that limits CORS application to specific URL
patterns. Suprnova mirrors this:

```rust,ignore
CorsConfig::allow_origins(["https://app.example"])
    .paths(["api/*", "sanctum/csrf-cookie"])
```

With no `paths` set, CORS runs on every request (Suprnova's default —
since the middleware is opt-in by registration). With at least one
pattern set, only matching requests get CORS treatment (both preflights
**and** actual-response decoration); everything else flows through
untouched.

Patterns use Laravel's `Str::is` semantics: `*` is a multi-segment
wildcard greedy across `/`. Leading `/` is normalized so `"api/*"` and
`"/api/*"` are equivalent.

```rust,ignore
"api/*"             // matches /api/users, /api/users/42
"api/*/posts"       // matches /api/v2/posts, /api/v1/posts
"sanctum/csrf-cookie" // exact-match literal
"*"                 // matches everything
```

## Skip via predicate

For request-shape predicates that don't fit a path pattern (skip based on
a header, only run CORS in production, skip during health checks), use
`skip_when`:

```rust,ignore
CorsConfig::any_origin()
    .skip_when(|req| req.header("X-Internal-Call").is_some())
    .skip_when(|req| req.path() == "/healthz")
```

Mirrors Laravel's `HandleCors::skipWhen(Closure)` but lives on the policy
rather than as global mutable state. Multiple `skip_when` callbacks can
be registered; any one returning `true` skips CORS.

## Methods, headers, exposed headers

```rust,ignore
CorsConfig::allow_origins(["https://app.example"])
    .methods(["GET", "POST", "DELETE"])           // default = GET/POST/PUT/PATCH/DELETE/OPTIONS/HEAD
    .allow_headers(["Content-Type", "X-CSRF-TOKEN"])  // restrict; default = reflect request
    .allow_any_headers()                          // explicit "reflect whatever was requested"
    .expose_headers(["X-Total-Count", "Link"])    // headers JS may read on the response
```

Laravel-named aliases (so `cors.php` users find what they expect):

- `allowed_methods(...)` ≡ `methods(...)`
- `allowed_headers(...)` ≡ `allow_headers(...)`
- `exposed_headers(...)` ≡ `expose_headers(...)`
- `allowed_origins_patterns(...)` ≡ `allow_origin_patterns(...)`
- `supports_credentials(...)` ≡ `allow_credentials(...)`

## Credentials and `*`

Per the Fetch spec, `Access-Control-Allow-Origin: *` is invalid together
with credentials — the browser rejects the response. With an explicit
origin list (`allow_origins([...])`) plus `allow_credentials(true)`,
the middleware echoes the specific request `Origin` rather than `*`,
and the policy works as expected.

**`any_origin() + allow_credentials(true)` panics at build time.** The
combination is a complete bypass of origin allowlisting: any attacker
page can make credentialed cross-origin requests and read responses.
Rather than emit the wrong header at runtime, the policy constructor
fails loud so the misconfiguration never reaches a running deployment.
Use an explicit allowlist instead:

```rust,ignore
// CORRECT — explicit allowlist with credentials.
CorsConfig::allow_origins(["https://app.example"]).allow_credentials(true)
// → on request with Origin: https://app.example
// → response: Access-Control-Allow-Origin: https://app.example
//             Access-Control-Allow-Credentials: true

// REJECTED at build time — panics with a remediation message.
// CorsConfig::any_origin().allow_credentials(true)
```

## Max-age

```rust,ignore
.max_age(Duration::from_secs(600))   // typed
.max_age_secs(600)                   // Laravel-style integer-seconds
```

`Access-Control-Max-Age` tells the browser how long it may cache the
preflight result. Higher = fewer preflight round-trips, slower policy
changes propagate.

## What the middleware actually emits

### Preflight (`OPTIONS` + `Access-Control-Request-Method`)

If the origin is allowed:

```
HTTP/1.1 204 No Content
Access-Control-Allow-Origin: <origin>
Access-Control-Allow-Credentials: true        // when credentials enabled
Access-Control-Allow-Methods: GET, POST, ...
Access-Control-Allow-Headers: <reflected or fixed>
Access-Control-Max-Age: 600                   // when set
Vary: Origin, Access-Control-Request-Method, Access-Control-Request-Headers
```

If the origin is disallowed: bare `204` + `Vary` (no `Access-Control-*`).
The browser's missing-header check produces the CORS error — matching
the `tower-http` convention.

### Actual cross-origin response

When the request has an `Origin` header and the origin is allowed:

```
Access-Control-Allow-Origin: <origin or *>
Access-Control-Allow-Credentials: true        // when enabled
Access-Control-Expose-Headers: X-Total, Link  // when configured
Vary: Origin                                  // only when not "*"
```

A `*` ACAO is identical for every origin, so no `Vary` is needed; a
specific origin varies per-origin so shared caches must key on it.

## Testing CORS handlers

CORS is browser-side enforced — the server still runs the handler even
when the origin is disallowed; it just doesn't decorate the response.
That's the testable behavior:

```rust,ignore
let (status, headers, body) = request_with_origin(
    "/api/data",
    "https://app.example",
).await;
assert_eq!(status, 200);
assert_eq!(
    headers.get("access-control-allow-origin"),
    Some(&"https://app.example".to_string()),
);
```

For a disallowed origin, the handler runs and the body comes back, but
the absence of `Access-Control-Allow-Origin` is what blocks the browser
from reading it.

## Laravel parity matrix

| Laravel `cors.php` | Suprnova builder |
| --- | --- |
| `paths` | `.paths([...])` |
| `allowed_methods` | `.methods([...])` / `.allowed_methods([...])` |
| `allowed_origins` | `CorsConfig::allow_origins([...])` |
| `allowed_origins_patterns` | `.allow_origin_patterns([...])` / `.allowed_origins_patterns([...])` |
| `allowed_headers` | `.allow_headers([...])` / `.allowed_headers([...])` |
| `exposed_headers` | `.expose_headers([...])` / `.exposed_headers([...])` |
| `max_age` | `.max_age(Duration)` / `.max_age_secs(u64)` |
| `supports_credentials` | `.allow_credentials(bool)` / `.supports_credentials(bool)` |
| `HandleCors::skipWhen(closure)` | `.skip_when(\|req\| ...)` |

The middleware is registered globally rather than the Laravel-style
"automatically installed for `paths`" — Suprnova's middleware chain is
explicit, see [Middleware](middleware.md) for the design.

### Why Suprnova diverges

Laravel's `HandleCors` is auto-attached to the kernel and reads its
policy from `config/cors.php`. The shape works for PHP because the
config array is the one place a request-per-process framework can
share configuration without re-evaluating it per request. Suprnova
exposes the same options as a typed `CorsConfig` builder you register
explicitly with `global_middleware!`, which keeps the middleware
chain visible in `bootstrap()` and lets the compiler enforce the
allowlist-vs-wildcard choice (no `Default` for `CorsConfig`, so you
can't accidentally ship `Access-Control-Allow-Origin: *` because you
forgot to fill in a config value).

The other divergence is that preflights reach the middleware even on
unrouted paths. Laravel routes `OPTIONS` through its router so the
preflight matches an `OPTIONS` route (auto-registered for each REST
route). Suprnova's router has no `OPTIONS` routes; instead the server
runs the global middleware chain on unmatched requests before
returning 404, so a globally-installed `CorsMiddleware` short-circuits
the preflight with `204` before the not-found path is taken. That's
why CORS *must* be installed globally — a per-route registration
would never see the preflight.

## Next

- [Middleware](middleware.md) — the trait, the chain, global vs
  per-route registration, terminable hooks
- [CSRF](csrf.md) — the other global middleware most apps install
  alongside CORS
- [Routing](routing.md) — how routes are matched (and why preflights
  don't match), plus the no-fallback path the global chain runs on
- [Request Lifecycle](lifecycle.md) — where CORS sits in the chain
  relative to session, CSRF, and the handler
- [Configuration](configuration.md) — typed config patterns for
  middleware that need environment-driven settings
