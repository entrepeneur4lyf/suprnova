# URL Generation

URLs are how your app references itself — every redirect, every email link,
every Inertia `<Link>` href, every signed download has to come from
somewhere. Hard-coding paths makes refactors painful and route renames
unsafe. Suprnova ships a small `url::` namespace and a sibling
`route()` helper that take a name plus parameters and give you back a
string, with percent-encoding handled, signature minting available, and
verification that matches Laravel's wire format byte-for-byte.

This chapter is the reference for the URL-generation surface. The
[Routing](routing.md) chapter covers how to declare routes and name them;
this one covers what you do with those names afterwards.

```rust
use suprnova::{route, url};

// Lookup by name → URL
let profile = route("users.show", &[("id", "42")]).unwrap();
//   "/users/42"

// Absolute URL against APP_URL
let absolute = url::to("/dashboard");
//   "https://app.test/dashboard"

// Signed link for password reset
let link = url::signed_route("password.reset", &[("token", reset_token)])?;
//   "/password/reset/xyz?signature=ab12..."

// Verify on the inbound request
if url::has_valid_signature(&request)? {
    // act on it
}
```

Everything in this chapter is re-exported under `suprnova::url::*` and
`suprnova::route` so consumer code never has to reach into the routing
module directly.

## Named routes

A name is a string label attached to a route at registration time. Once a
name exists, `route(name, params)` resolves it back to a URL pattern and
substitutes the parameters. Names live in a single process-global
registry — there is one `name → path` table per running binary, not one
per `Router`.

```rust
use suprnova::{routes, get, post};

routes! {
    get!("/", controllers::home::index).name("home"),
    get!("/users/{id}", controllers::users::show).name("users.show"),
    post!("/users", controllers::users::store).name("users.store"),
}
```

The `.name(...)` call registers `"users.show" → "/users/{id}"`. From
that point on, anywhere in the process can resolve the name:

```rust
use suprnova::route;

let url = route("users.show", &[("id", "42")]);
// Some("/users/42")

let missing = route("does.not.exist", &[]);
// None
```

Re-registering the same `(name, path)` pair is idempotent — useful when
route registration runs more than once during boot. Registering a name
under a *different* path panics; that collision is a security-shaped
bug because helpers like `Redirect::route` would silently target
whichever side won the race.

### The lookup helpers

| Function | Returns | When the route is missing |
|---|---|---|
| `route(name, params)` | `Option<String>` | `None` |
| `route_with_params(name, params_map)` | `Option<String>` | `None` |
| `try_route(name, params)` | `Result<String, RouteUrlError>` | `Err(NameNotFound)` |
| `try_route_with_params(name, params_map)` | `Result<String, RouteUrlError>` | `Err(NameNotFound)` |

The lenient `route` / `route_with_params` pair leaves any unfilled
`{placeholder}` segment verbatim in the output — fine for debug logs,
unsafe to ship to a browser. The strict `try_route` / `try_route_with_params`
pair returns `RouteUrlError::MissingParams { name, missing }` listing the
unfilled placeholders so the caller can fail loudly instead of redirecting
a user to `/users/{id}`.

```rust
use suprnova::routing::{try_route, RouteUrlError};

match try_route("users.show", &[]) {
    Ok(url) => /* safe to redirect */,
    Err(RouteUrlError::MissingParams { name, missing }) => {
        // missing == vec!["id"]
        return Err(FrameworkError::internal(
            format!("cannot build URL for {name}: missing {missing:?}"),
        ));
    }
    Err(RouteUrlError::NameNotFound(name)) => {
        return Err(FrameworkError::internal(format!("unknown route: {name}")));
    }
}
```

`Redirect::route` uses `try_route_with_params` under the hood for exactly
this reason — a redirect with a raw `{id}` in the `Location` header would
be worse than failing.

### Percent-encoding is automatic

Parameter values are encoded per RFC 3986 path-segment rules before they
are substituted in. That covers the gen-delims and sub-delims
(`/ ? # [ ] @ ! $ & ' ( ) * + , ; =`), control characters, space, and
`%` itself. Unreserved characters (`A-Z a-z 0-9 - _ . ~`) pass through
unchanged.

```rust
use suprnova::route;

// A slug containing a slash is contained in one segment:
route("posts.show", &[("slug", "hello/world")]);
// Some("/posts/hello%2Fworld")

// Path traversal attempts can't escape the segment:
route("users.show", &[("id", "../../etc/passwd")]);
// Some("/users/..%2F..%2Fetc%2Fpasswd")

// Real Unicode passes through untouched:
route("users.show", &[("id", "user-é-42")]);
// Some("/users/user-%C3%A9-42")
```

The matching side preserves this round-trip — a request to
`/posts/hello%2Fworld` matches the `/posts/{slug}` route and a handler
reading `req.param("slug")` sees `"hello/world"`, decoded. Encode at the
boundary, decode at the boundary; never see the raw bytes in handler code.

### Reverse lookup

When you have a matched route pattern and want the registered name —
e.g. for logging or for `Request::route_is("users.show")` checks — use
`route_name_for_pattern`:

```rust
use suprnova::routing::route_name_for_pattern;

let name = route_name_for_pattern("/users/{id}");
// Some("users.show")
```

This is an O(n) scan over the name registry. n is the number of
registered names; even at four-digit route counts the cost is negligible
compared to the surrounding request lifecycle. The function is exposed
for tooling and middleware — `Request::route_is` already calls it for
you when you compare against a named route in a handler.

## Absolute URLs

For everything else — building emails, sharing URLs, sending Open Graph
metadata — you want an absolute URL with the right scheme and host.
`url::to` joins a path to `APP_URL`:

```rust
use suprnova::url;

// In env: APP_URL=https://app.example.com
let url = url::to("/about");
// "https://app.example.com/about"

// Already-absolute URLs pass through unchanged:
let cdn = url::to("https://cdn.example/asset.js");
// "https://cdn.example/asset.js"

let proto_relative = url::to("//cdn.example/asset.js");
// "//cdn.example/asset.js"
```

The host, scheme, and port all come from `APP_URL`. If `APP_URL` is
`http://localhost:8080`, then `url::to("/foo")` yields
`"http://localhost:8080/foo"`. The trailing slash on `APP_URL` is
normalised away so you never end up with `https://host//path`.

### Forcing HTTPS

`url::secure(path)` builds the same absolute URL but upgrades the scheme
to `https://` even if `APP_URL` is `http://`:

```rust
use suprnova::url;

// In env: APP_URL=http://app.example.com
url::secure("/login");
// "https://app.example.com/login"
```

In production you typically set `APP_URL` to your HTTPS host once and
never call `secure` directly — the upgrade is for environments where
local development runs over HTTP but a specific link must be HTTPS
(e.g. a callback URL embedded in a payment session).

### Reading the current URL

Inside a handler, the request itself is the source of truth:

```rust
use suprnova::url;

async fn breadcrumbs(req: Request) -> Response {
    let here = url::current(&req);       // "/posts/42?expand=author"
    let full = url::full(&req);          // "https://app.test/posts/42?expand=author"
    let back = url::previous("/");        // session-recorded previous URL
    // ...
}
```

| Helper | Returns | Source |
|---|---|---|
| `url::current(&req)` | path + query of this request | The current `Request` |
| `url::full(&req)` | absolute URL of this request | `APP_URL` + `current(&req)` |
| `url::previous(fallback)` | previous URL recorded by the session middleware | `_previous.url` in the session, or `fallback` |

`previous` is what backs `Redirect::back` — the session middleware
records the URL of every successful HTML GET so a form `POST` can bounce
back to the page that submitted it. Inertia partials, JSON-API requests
(`Accept: application/json` without `text/html`), and non-2xx/3xx
responses are skipped so you never bounce back to an intermediate
endpoint the user never saw.

## Signed URLs

Signed URLs let you mint a URL that proves it came from your server,
without storing the URL anywhere. The signature is HMAC-SHA256 over the
canonical form of the URL using your `APP_KEY`; the server recomputes
the HMAC on the inbound request and accepts only matching signatures.

Reach for signed URLs when:

- **Email-delivered links** — password reset, email verification,
  invite-by-email, magic-link login. The URL has to survive a round trip
  through an inbox without being storable as opaque state.
- **Ephemeral downloads** — "your CSV export is ready" links that expire
  in 24 hours, signed S3 alternatives where you want the URL to remain
  on your domain.
- **Webhooks pointing back at you** — third-party callbacks that should
  refuse forged calls without requiring a database lookup per request.

```rust
use suprnova::url;
use chrono::Utc;

// Permanent signed URL — never expires.
let link = url::signed_route(
    "password.reset",
    &[("user", user_id), ("token", token)],
)?;
// "/password/reset/42/xyz?signature=ab12cd34..."

// Temporary signed URL — expires one hour from now.
let expires_at = Utc::now().timestamp() + 3600;
let link = url::temporary_signed_route(
    "verify.email",
    &[("user", user_id)],
    expires_at,
)?;
// "/verify/email/42?expires=1748803600&signature=def012..."
```

Note that `expires_at_epoch_seconds` is an **absolute UNIX timestamp**,
not a duration. Compute it at the call site:

```rust
let one_hour_from_now = chrono::Utc::now().timestamp() + 3600;
let one_day_from_now  = chrono::Utc::now().timestamp() + 86_400;
```

That keeps the helper signature small and lets you reuse the same
function for both relative-from-now and explicit-absolute deadlines.

### Verifying

On the inbound side, you verify the signature against the live request:

```rust
use suprnova::{url, Request, Response, HttpResponse};

async fn reset(req: Request) -> Response {
    if !url::has_valid_signature(&req)? {
        return HttpResponse::text("Invalid or expired link").status(403);
    }
    // Signature is good and not expired — proceed.
    let user_id = req.param("user").unwrap();
    // ...
}
```

`has_valid_signature` returns `true` only when the HMAC matches AND the
URL is not expired. For the three-way distinction between *invalid*,
*expired*, and *valid*, use `signature_verdict`:

```rust
use suprnova::{url, routing::SignatureVerdict};

async fn reset(req: Request) -> Response {
    match url::signature_verdict(&req)? {
        SignatureVerdict::Valid => {
            // Proceed.
        }
        SignatureVerdict::Expired => {
            // Bounce the user to a page that explains the link expired
            // and offers to send a fresh one.
            return Redirect::to("/password/reset-expired").into();
        }
        SignatureVerdict::Invalid => {
            // Render a generic 403 — don't leak whether the signature
            // was malformed, missing, or just wrong.
            return HttpResponse::text("Invalid link").status(403);
        }
    }
    // ...
}
```

`signature_has_not_expired(&req)` is the inverse helper that returns
`true` when the URL is either valid or invalid-but-not-expired — useful
when expiration is the only thing you care about. The Laravel sibling
behaves the same way: a URL with no `expires` query parameter is "never
expired" by definition.

### Signing arbitrary URLs

If the URL you want to sign doesn't come from a registered named route
— a callback URL handed to you by a third party, a path constructed
dynamically at runtime — use `signed_url` directly:

```rust
use suprnova::url;

let callback = url::signed_url(
    "/webhooks/stripe/callback?order=42",
    Some(chrono::Utc::now().timestamp() + 600),  // 10-minute expiry
)?;
```

Pass `None` for the expiration to mint a permanent signature. The verify
side is the same — `has_valid_signature(&req)` doesn't care whether the
URL was minted from a named route or from a raw path.

### Wire format

Two URLs that differ only by query-parameter order produce identical
signatures because the canonical form sorts query pairs lexicographically
before hashing. That matters because clients sometimes reorder query
parameters in transit (proxies, link previewers, mobile email apps), and
a signed URL that breaks under reordering would be unusable.

| Component | Value |
|---|---|
| Algorithm | HMAC-SHA256 |
| Key | Active `APP_KEY` raw bytes |
| Payload | `path?<sorted-query>` (omit `?` when no params) |
| Encoding | Hex-encoded 64-character digest |
| Comparison | Constant-time via `subtle::ConstantTimeEq` |
| Reserved keys | `signature`, `expires` |

The HMAC payload excludes any pre-existing `signature` query parameter
(so signing-over-signing is a no-op) and re-emits a fresh `expires` value
from the call arguments. A client that strips or rewrites the `expires`
breaks the signature; a client that strips the `signature` fails as
`Invalid`. Both fail closed.

The fragment (`#section`) is stripped from the canonical form because
browsers never transmit fragments back to the server. Signing over a
fragment would invalidate every link the moment a client appended an
anchor — `?signature=...#docs` would not verify on the server side.

### Reserved query parameters

`signature` and `expires` are reserved query-parameter names. A route
that legitimately expects a query parameter called `signature` or
`expires` would collide with the signed-URL machinery, and the verifier
would mis-attribute the value. Either rename the parameter or wrap the
route's incoming parameters under a different namespace.

```rust
// Bad — `signature` collides with the reserved name.
get!("/api/check", check)  // takes ?signature=hash

// Good — namespace it.
get!("/api/check", check)  // takes ?body_signature=hash
```

The constants are exposed for symmetry with the Laravel wire format:

```rust
use suprnova::routing::{SIGNATURE_KEY, EXPIRES_KEY};
// SIGNATURE_KEY == "signature"
// EXPIRES_KEY   == "expires"
```

### Key rotation

Signed URLs use the same `APP_KEY` that powers `Crypt::encrypt` and
session-cookie integrity. Rotating `APP_KEY` invalidates every
previously-minted signature in flight — an in-flight password-reset
email becomes a 403 the next time the user clicks it.

For most applications that is the correct behaviour. If you need
graceful rotation with overlap (so old links keep working through a
deployment window), use `APP_KEY_PREVIOUS` to carry the prior key
forward; the keyring tries every installed key on verification. See the
[Hashing](hashing.md) chapter for the full keyring story.

## Errors and edge cases

A handful of failure modes are worth knowing about:

- **`route(name, ...)` returns `None`** when the name is not registered.
  This is the lenient surface — silent failure is intentional so calling
  code can fall back to a default. Use `try_route` for a loud failure.
- **`try_route` returns `Err(NameNotFound)`** for an unknown name and
  `Err(MissingParams { name, missing })` when a required `{placeholder}`
  has no matching value.
- **`url::signed_route` and friends return `FrameworkError`** when the
  encryption key isn't installed (e.g. you forgot `APP_KEY` in `.env`).
  This fails at boot in production because `Crypt::init` runs during
  `Server::from_config`; the error path here exists to surface
  misconfiguration loudly instead of producing unverifiable links.
- **`has_valid_signature` returns `Ok(false)`**, not `Err`, for an
  invalid or expired signature. The `FrameworkError` variant is reserved
  for "the server can't even check" failures (missing key).
- **A signed URL with a tampered `expires`** verifies as `Invalid`, not
  `Expired`. The HMAC payload includes the `expires` value, so changing
  it breaks the signature first.

```rust
use suprnova::{routing::SignatureVerdict, url};

// All of these are Invalid, not Expired:
url::signature_verdict(&req)?;  // signature query param missing
url::signature_verdict(&req)?;  // signature is non-hex junk
url::signature_verdict(&req)?;  // path was tampered (/orders/1 → /orders/2)
url::signature_verdict(&req)?;  // any query param value was tampered
url::signature_verdict(&req)?;  // expires value was tampered

// This is Expired:
url::signature_verdict(&req)?;  // valid HMAC, but now > expires
```

## Why Suprnova diverges

Laravel's `URL` facade carries `asset()`, `secureAsset()`, `assetFrom()`,
and `action()`. Suprnova ships none of them — for deliberate reasons.

**Assets**. Suprnova's frontend story is Vite plus the filesystem disks
([Filesystem](filesystem.md)), not a stand-alone asset helper. Vite's
`@vite('resources/app.ts')` directive (or the Inertia adapter's equivalent)
emits the correct hashed URLs in production and the dev-server URL in
development. Building a parallel `URL::asset()` channel would split the
asset story across two systems that have to agree about hashing,
versioning, and which manifest is authoritative. The Vite side already
won that responsibility.

**Action routing**. Laravel's `action('UserController@show', ['id' => 1])`
relies on PHP class-string routing — controllers are classes with
methods, and the framework can reverse-look-up an `action` string. Rust
handlers are free functions. The closest analogue is named routes, and
`route("users.show", &[("id", "1")])` is already the right interface.
Re-introducing action-string routing on top of Rust handler types would
add nothing real over named routes.

**`URL::forceScheme()` / `URL::forceRootUrl()`**. Laravel exposes these
for tests and for sites behind reverse proxies that don't pass
`X-Forwarded-Proto`. Suprnova handles both cases by configuration:
`APP_URL` carries the canonical host and scheme; for proxy environments,
the trusted-proxy middleware ([Middleware](middleware.md)) reads
`X-Forwarded-*` headers and updates the request URL before it reaches
your handler. There's nothing for `forceScheme` to override — `APP_URL`
already says what the scheme is.

What does land here is the user-facing shape consumers reach for, with
the same Laravel-shaped names where they translate cleanly. The trim is
intentional, not an oversight.

## Next

- [Routing](routing.md) — declaring routes, naming them, route groups,
  resource routing, and the full per-method matching surface
- [Responses](responses.md) — `Redirect::route`, `Redirect::signed_route`,
  `Redirect::back`, and the rest of the redirect helper family that
  consumes URL generation
- [Hashing](hashing.md) — `APP_KEY` lifecycle, key rotation, and the
  shared keyring that backs URL signing alongside encryption
- [Auth flows](auth-flows.md) — the production users of signed URLs:
  password reset, email verification, and remember-me cookies
- [Requests](requests.md) — `Request::path`, `Request::query`,
  `Request::route_is`, and the reverse side of every helper in this
  chapter
