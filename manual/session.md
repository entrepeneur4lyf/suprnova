# Sessions

The session is the per-user key/value bag that survives across requests
on the same browser. Suprnova ships a database-backed driver out of the
box, wires it in via `SessionMiddleware`, and exposes the active session
through two free functions — `session()` for reads, `session_mut()` for
writes. Use it whenever a value should outlive one request but not be
something the URL or a JWT should carry.

## How a request sees the session

`SessionMiddleware` runs on every request and does five things in order:

1. Reads the session id from the `suprnova_session` cookie (AES-256-GCM
   encrypted; tampered or undecryptable cookies mint a fresh id rather
   than fail loud, matching Laravel's posture).
2. Loads `SessionData` from the store (database by default). A missing
   row creates a new session in memory with a fresh CSRF token. A store
   read error logs `warn!` and degrades to a fresh session — the
   request still serves.
3. Ages flash data: `_flash.old.*` is dropped, `_flash.new.*` is
   renamed to `_flash.old.*`. After this step, anything the previous
   request flashed is readable; anything this request flashes will be
   readable next time.
4. Binds the session into a task-local slot for the duration of the
   handler. `session()` and `session_mut()` look the slot up.
5. After the handler returns, persists the session (always — even an
   unmodified session gets its `last_activity` bumped so sliding
   expiration works), attaches the encrypted session cookie, and
   drains any pending out-of-band cookies (e.g. a freshly-rotated
   remember-me).

Step 5 has one safety guarantee worth pulling out: **if the session
was modified this request and the store write fails, the response is
replaced with a 500.** Returning the handler's success would mean
handing the client a cookie for state the database never recorded —
the next request would load an empty session and the mutation
(login, CSRF rotation, flash) would silently vanish. Read-only
requests that fail only on the `last_activity` touch log `warn!` and
pass through.

## Reading the session

```rust
use suprnova::session::session;

if let Some(s) = session() {
    let user_id: Option<String> = s.get("preferred_username");
    if s.has("cart") {
        // ...
    }
    if s.missing("locale") {
        // first visit
    }
}
```

`session()` clones the current `SessionData`. Returns `None` outside a
request scope (a unit test that didn't install the middleware, a CLI
subcommand). For a typed value, `get::<T>` deserializes from the
underlying JSON; on a missing key or wrong type, you get `None` and no
panic.

## Writing the session

`session_mut` takes a closure that receives `&mut SessionData`:

```rust
use suprnova::session::session_mut;

session_mut(|s| {
    s.put("locale", "en");
    s.put("preferences", serde_json::json!({
        "theme": "dark",
        "notifications": true,
    }));
    s.forget("legacy_key");
});
```

The closure is sync — guards on the underlying lock drop before any
`.await`, so this composes inside async handlers without holding the
lock across suspensions. Anything you serialize must implement
`Serialize`; deserialization on `get` requires `DeserializeOwned`.

The closure form (rather than returning a guard) is deliberate. Futures
in Tokio can resume on a different worker thread than the one they
started on, so the session has to live in a `task_local!` slot and be
borrowed through a scope-bound critical section. The `|s|` shape makes
that boundary explicit and stops you accidentally holding a mutex guard
across an `.await`.

## Flash data

Flash values are visible for **one** subsequent request, then disappear.
The usual pattern: a controller writes a flash, returns a redirect, the
next page renders the flash.

```rust
use suprnova::session::session_mut;

session_mut(|s| s.flash("status", "Profile updated."));
```

On the next request:

```rust
use suprnova::session::session_mut;

let status: Option<String> = session_mut(|s| s.get_flash("status"));
```

`get_flash` removes the value as it returns it. For the read-without-
consume variant use `get::<String>("_flash.old.status")`, but the
consuming form is what controllers usually want.

The full flash surface from Laravel is available:

- `flash(key, value)` — write for next request
- `now(key, value)` — write for the current request only
- `reflash()` — re-flash everything currently visible for one more turn
- `keep(&["k1", "k2"])` — re-flash a specific subset
- `flash_input(map)` / `old_input()` / `get_old_input(key)` — the
  form-input bag used by `Redirect::with_input` / `old()` helpers

## Regenerate and invalidate

After a credential change (login, password reset, 2FA pass) you rotate
the session id so a fixated id from before the change is no longer
valid:

```rust
use suprnova::session::{regenerate_session_id, regenerate_csrf_token};

regenerate_session_id();        // new id, same data
regenerate_csrf_token();        // new CSRF token, same id and data
```

To clear the session entirely (logout):

```rust
use suprnova::session::invalidate_session;

invalidate_session();           // clears data + mints fresh CSRF token
```

For a security event that needs to revoke every session for a user
(password reset elsewhere, account recovery, admin force-logout):

```rust
use suprnova::session::destroy_all_for_user;

let rows = destroy_all_for_user("user-42").await?;
tracing::info!(revoked = rows, "all sessions destroyed");
```

This wraps `SessionStore::destroy_for_user` against the framework's
default `DatabaseSessionDriver`. If you bound a custom store, call
`destroy_for_user` on it directly.

## Authentication helpers

`auth_user_id()` returns the currently-authenticated user id (consulting
request-scoped auth state first, falling back to the persisted session
field):

```rust
use suprnova::session::{auth_user_id, is_authenticated};

if is_authenticated() {
    let uid = auth_user_id().expect("just checked");
    // ...
}
```

You normally drive auth through the [Auth](authentication.md) facade —
`Auth::login`, `Auth::logout`, `Auth::user()`. The session helpers are
the low-level layer those facades sit on; reach for them when you need
to inspect the raw session or when implementing your own guard.

## Other operations

The `SessionData` API mirrors Laravel's `Store` surface:

| Method | What it does |
|---|---|
| `get::<T>(key)` | typed read |
| `put(key, value)` | typed write |
| `forget(key)` | remove a single key |
| `forget_many(&[..])` | remove many keys |
| `flush()` | clear all data (keeps id) |
| `has(key)` / `missing(key)` | presence check |
| `has_any(&[..])` / `has_all(&[..])` | bulk presence |
| `all()` | borrow the underlying map |
| `only(&[..])` / `except(&[..])` | filtered clones |
| `pull::<T>(key)` | get-and-forget in one shot |
| `push(key, value)` | append to an array value |
| `increment(key, n)` / `decrement(key, n)` | integer counters |
| `remember::<T>(key, \|\| default())` | get-or-compute-and-put |
| `replace(&[(k, v), ..])` | flush then bulk put |
| `put_many(&[(k, v), ..])` | merge bulk put |
| `previous_url()` / `set_previous_url(url)` | what `Redirect::back` reads |
| `password_confirmed()` / `password_confirmed_at()` | "user confirmed password just now" timestamp |

Reach for these inside `session_mut` for mutating ops, `session()`
for reads. The `previous_url` slot is populated automatically by the
middleware on successful GET HTML responses, so `redirect()->back()`
works without you doing anything.

## Configuration

Configure sessions via environment variables — `SessionConfig::from_env`
reads them at boot:

```env
# Lifetime in minutes. Drives both the row TTL and the cookie Max-Age.
SESSION_LIFETIME=120

# Cookie name on the client.
SESSION_COOKIE=suprnova_session

# Cookie attributes
SESSION_SECURE=true          # require HTTPS; DEFAULT IS true
SESSION_PATH=/
SESSION_DOMAIN=.example.com  # optional; unset = host-only
SESSION_SAME_SITE=Lax        # Lax | Strict | None
SESSION_PARTITIONED=false    # CHIPS opt-in
SESSION_EXPIRE_ON_CLOSE=false # true → omit Max-Age, browser drops on close

# Named DB connection for the session store (optional)
SESSION_CONNECTION=sessions

# Remember-me token/cookie lifetime in minutes (default 30 days)
REMEMBER_LIFETIME=43200
```

A few defaults worth flagging:

- **`SESSION_SECURE` defaults to `true`.** Sessions sent over plain
  HTTP would be a credential-leak hazard, so the secure flag is on by
  default. For local development over HTTP, set `SESSION_SECURE=false`
  in your local `.env`.
- **`HttpOnly` is always on.** There is no knob to disable it —
  exposing the session cookie to JavaScript forfeits the primary XSS
  protection and there is no legitimate modern reason to want it.
- **`SameSite` defaults to `Lax`.** `Strict` blocks the session on
  most cross-site GET navigations (including back-links from email);
  `Lax` is the usual right answer.

For programmatic config use the fluent builder:

```rust
use std::time::Duration;
use suprnova::SessionConfig;

let config = SessionConfig::new()
    .lifetime(Duration::from_secs(60 * 60))      // 1 hour
    .cookie_name("myapp_session")
    .secure(true)
    .domain(".example.com")
    .remember_lifetime(Duration::from_secs(30 * 24 * 60 * 60));
```

## Wiring it up

`SessionMiddleware` is installed as a global middleware in your app's
bootstrap. The middleware ordering matters: session must come before
[CSRF](csrf.md), since CSRF reads the per-session token.

```rust
use std::sync::Arc;
use suprnova::{global_middleware, CsrfMiddleware, SessionConfig, SessionMiddleware};

pub async fn bootstrap() {
    let config = SessionConfig::from_env();

    // `install` spawns a once-per-hour GC background task as well.
    // Use `SessionMiddleware::new(config)` if you'd rather schedule GC
    // yourself via `Schedule`.
    global_middleware!(SessionMiddleware::install(config));

    global_middleware!(CsrfMiddleware::new());
}
```

`SessionMiddleware::install` runs garbage collection once an hour in a
spawned Tokio task. The variant `install_with_gc(config, interval)`
takes a custom interval; `new(config)` skips the GC task (useful if
you'd rather call `gc()` from a [Schedule](scheduling.md) entry).

To use a non-database store — for tests, or for a Redis-backed driver
you write yourself — implement `SessionStore` and pass it via
`with_store`:

```rust
use std::sync::Arc;
use suprnova::{SessionConfig, SessionMiddleware, SessionStore};

let store: Arc<dyn SessionStore> = Arc::new(MyRedisStore::new());
let mw = SessionMiddleware::with_store(SessionConfig::from_env(), store);
```

## The sessions table

The default driver expects a `sessions` table with this shape (the
SeaORM entity in `framework/src/session/driver/database.rs` is the
source of truth):

| Column | Type | Notes |
|---|---|---|
| `id` | VARCHAR PK | 40-char lowercase alphanumeric session id |
| `user_id` | VARCHAR NULL | authenticated user id (string, supports opaque ids) |
| `payload` | TEXT | JSON-serialized session data map |
| `csrf_token` | VARCHAR | per-session CSRF token |
| `last_activity` | TIMESTAMP | last access; drives expiry + GC |

Two indexes ship alongside the table: `idx_sessions_user_id` (for
`destroy_for_user`) and `idx_sessions_last_activity` (for `gc()`).

A scaffolded app includes a `create_sessions_table` migration that
matches this shape. If you bring your own migrations, mirror the column
names exactly — SeaORM resolves them positionally and a renamed column
won't match.

### Why Suprnova diverges

Two places where Laravel made a PHP-shaped choice that Tokio lets us
make differently:

**Garbage collection.** Laravel runs a 2/100 lottery on every request:
each request has a 2% chance of triggering session GC inline. It works
on PHP because every request spawns a fresh process anyway. On Tokio
we have long-lived workers, so `SessionMiddleware::install` spawns one
background task that calls `gc()` on a fixed interval. No per-request
overhead, no probabilistic surprise — explicit scheduling instead of a
lottery.

**Closure-form `session_mut`.** Laravel hands you `$request->session()`
and lets you call methods on it. We don't, because handlers in Suprnova
are futures and a future can resume on a different worker thread than
it started on. The session lives in a Tokio `task_local!` slot, which
means borrowed access has to happen inside a scope. The closure form
makes that scope explicit and statically prevents the mistake of
holding a mutex guard across `.await`.

**Fail-closed on dirty writes.** A failed write of an unmodified
session logs `warn!` and lets the request through (the user-visible
state is intact). A failed write of a *modified* session — login,
flash, CSRF rotation — returns 500. Silently handing the client a
cookie for state the store never recorded would make a "successful"
login vanish on the very next request; better to surface the failure
loudly.

## Next

- [Authentication](authentication.md) — `Auth::login`, guards, the user provider chain
- [Auth Flows](auth-flows.md) — password reset, 2FA, brute-force throttling, remember-me
- [CSRF](csrf.md) — how the session's CSRF token gets checked on writes
- [Middleware](middleware.md) — writing your own middleware that reads or writes the session
- [Request Lifecycle](lifecycle.md) — where `SessionMiddleware` sits in the chain
