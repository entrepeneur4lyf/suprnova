# Authorization

Authentication answers _"who are you?"_ — authorization answers _"are you
allowed to do this?"_ suprnova ships a Laravel-shaped `Gate` facade plus the
`#[policy]` macro for resource-oriented wiring, with sync and async variants
of every check so the same surface works whether your policy body needs a DB
hit or just a struct-field comparison.

## Quick start

```rust
use suprnova::{Authorizable, Gate};

#[derive(Debug)]
struct User { id: i64, is_admin: bool }
#[derive(Debug)]
struct Post { id: i64, author_id: i64, is_public: bool }

// Lets users opt into the `user.can(action, &resource)` ergonomics.
impl Authorizable for User {}

// Wire one ability:
Gate::define::<User, Post>("update", |user, post| {
    user.is_admin || post.author_id == user.id
});

let alice = User { id: 1, is_admin: false };
let own_post = Post { id: 10, author_id: 1, is_public: false };
let foreign_post = Post { id: 11, author_id: 99, is_public: false };

assert!(alice.can("update", &own_post));
assert!(alice.cannot("update", &foreign_post));

// Return 403 directly from a handler:
alice.authorize("update", &foreign_post)?;
```

## The `Gate` surface

### Defining abilities

```rust
// Sync closure — fastest path, no allocation.
Gate::define::<User, Post>("view", |user, post| post.is_public || user.id == post.author_id);

// Async closure — the future must be owned (no borrows past closure return).
Gate::define_async::<User, Post, _, _>("publish", |user, post| {
    let user_is_admin = user.is_admin;
    let post_id = post.id;
    async move {
        // ...DB lookup, RPC call, etc.
        user_is_admin || check_publish_permission(post_id).await
    }
});
```

Type-erased internally; the registry keys on `(action, TypeId<U>, TypeId<R>)`.
A `User` action gate and a `Comment` action gate of the same name live
independently — `Gate::has::<User, Post>("publish")` and
`Gate::has::<User, Comment>("publish")` answer separately.

### Checking abilities

| Method | Returns | Use |
|---|---|---|
| `Gate::allows(action, &user, &resource)` | `bool` | Quick branch |
| `Gate::denies(action, &user, &resource)` | `bool` | Inverse |
| `Gate::authorize(action, &user, &resource)` | `Result<(), FrameworkError>` | 403 on a bare deny; a rich denial carries its own status/message (see [Rich decisions](#rich-decisions-response-inspect-raw)) — short-circuits a handler with `?` |
| `Gate::inspect(action, &user, &resource)` | `Response` | Full decision: `allowed` + `message` + `code` + HTTP `status` |
| `Gate::raw(action, &user, &resource)` | `Option<Response>` | Like `inspect`, but `None` = no rule defined (vs an explicit deny) |
| `Gate::any(&[...], &user, &resource)` | `bool` | True if any allow |
| `Gate::none(&[...], &user, &resource)` | `bool` | True if none allow |
| `Gate::check(&[...], &user, &resource)` | `bool` | True if all allow |

Every method has an `_async` sibling that works for both sync- and
async-registered gates, so handlers don't need to know which kind of
closure backs the action.

### Introspection

```rust
// Is an ability defined?
Gate::has::<User, Post>("publish");  // bool

// What abilities exist? (sorted + deduped by action name)
let all: Vec<String> = Gate::abilities();
```

`abilities()` dedupes across resource types: registering `"view"` for
both `User`-on-`Post` and `User`-on-`Comment` yields a single `"view"`
entry. Useful for admin pickers and Inertia shared-data.

### Missing-gate semantics

Calling `allows` / `denies` / `authorize` on an action that was never
registered **defaults to deny**. Same for calling the sync API on an
async-registered gate (the sync path can't await — defaulting deny
surfaces the bug in logs via `tracing::warn!` rather than silently
passing). Async-registered gates respond correctly from the
`_async` paths.

## Policies with `#[policy]`

When a resource type has several abilities, group them into a policy struct
and let `#[policy]` register every method as a gate:

```rust
use suprnova::policy;
use suprnova::authorization::Response;

struct User { id: i64, is_admin: bool }
struct Post { id: i64, author_id: i64, is_public: bool }
struct PostPolicy;

#[policy(User, Post)]
impl PostPolicy {
    // A `-> bool` method is a plain allow/deny gate.
    fn view_any(_user: &User, _post: &Post) -> bool {
        true // anyone can list posts
    }
    fn view(user: &User, post: &Post) -> bool {
        post.is_public || post.author_id == user.id || user.is_admin
    }

    // A `-> Response` method can carry a message + HTTP status on denial.
    fn update(user: &User, post: &Post) -> Response {
        if post.author_id == user.id || user.is_admin {
            Response::allow()
        } else {
            Response::deny_with("You may only edit your own posts.")
        }
    }
    fn delete(user: &User, post: &Post) -> Response {
        if user.is_admin {
            Response::allow()
        } else {
            Response::deny_as_not_found() // hide the post from non-admins
        }
    }
}
```

Each method becomes one `inventory::submit!`. `Server::serve` drains the
inventory via `init_policies()` at boot, so by the time the first request
arrives every action is registered. `init_policies()` is also public and
idempotent — call it manually in tests.

Policy methods are stateless associated functions taking `(user, resource)` —
the same shape as Laravel's `update(User $user, Post $post)`, where `$this` is
the stateless policy object. Every method takes both arguments for a uniform
gate signature; `view_any` / `create` simply ignore the resource (`_post`).
Methods you don't write aren't registered, and an unregistered action
default-denies.

### Method-name → action mapping

Method name is used directly as the action's verb segment, with the
resource kebab-cased and suffixed:

| Method | Action |
|---|---|
| `view` on `Post` | `"view-post"` |
| `view_any` on `Post` | `"view_any-post"` |
| `force_delete` on `UserProfile` | `"force_delete-user-profile"` |

This diverges from Laravel's camelCase action names (`viewAny`,
`forceDelete`) to keep the Rust surface idiomatic — every action
string mirrors the method identifier you'd autocomplete in your
editor.

### Return type: `bool` or `Response`

A policy method's return type selects how it registers — and what a denial
can carry:

| Return type | Registers via | Denial surfaces as |
|---|---|---|
| `bool` | `Gate::define` | bare `403` (`This action is unauthorized.`) |
| `Response` | `Gate::define_with` | the message, code, and HTTP status the `Response` carries |

Return `bool` for a simple yes/no. Return a `Response` (imported from
`suprnova::authorization::Response`) when a denial should carry a reason or a
non-403 status — `Response::deny_with("…")` for a message, or
`Response::deny_as_not_found()` to answer `404` and hide the resource's
existence. Both compile to the same type-erased gate (a `bool` is wrapped into
a bare allow/deny). Any other return type — or a missing one — is a compile
error.

## The `Authorizable` trait

Drop-in user-side sugar for the `Gate` calls:

```rust
use suprnova::Authorizable;

impl Authorizable for User {}

// Sync sugar
if alice.can("update", &post)    { /* ... */ }
if alice.cannot("delete", &post) { /* ... */ }
alice.authorize("update", &post)?;  // 403 on deny

// Async sugar
if alice.can_async("publish", &post).await    { /* ... */ }
alice.authorize_async("publish", &post).await?;
```

Every method has a default body that delegates to the matching `Gate`
method, so `impl Authorizable for User {}` (no body) is enough.
Opt-in rather than blanket-impl: not every type that can be passed to
`Gate::allows` is meant to be the subject of `.can` — most often
it's your application's `User`.

## Composition patterns

### Gating route groups

```rust
use suprnova::{group, get, AuthMiddleware};

// Middleware checks the auth user; the handler authorizes the action.
group!("/posts")
    .middleware(AuthMiddleware::new())
    .routes([
        get!("/{id}/edit", edit_form),
    ]);

async fn edit_form(req: Request) -> Response {
    let user: User = Auth::user_as().await?.ok_or(FrameworkError::Unauthorized)?;
    let post = Post::find(req.path_param("id")?).await?;
    user.authorize("update", &post)?;
    // ... render edit form
}
```

### Many-action checks

A "list all the things this user can do on this resource" page:

```rust
let actions = ["view", "update", "delete", "restore", "force_delete"];
let mut allowed = Vec::new();
for action in &actions {
    if user.can(action, &post) {
        allowed.push(*action);
    }
}
// Or short-circuit:
let can_do_anything = Gate::any(&actions, &user, &post);
let is_locked_out   = Gate::none(&actions, &user, &post);
```

### Multi-gate authorization

```rust
// Only allow if the user can do ALL of these actions on the resource.
Gate::authorize_async("publish", &user, &post).await?;
if Gate::check_async(&["update", "view"], &user, &post).await {
    // Combine checks.
}
```

## Async semantics

`Gate::define_async`'s closure must return an **owned** future — the
type-erased registry cannot let `&user` or `&resource` references
outlive the closure return. Copy or clone any fields you need inside
the `async move {}` block before returning it:

```rust
Gate::define_async::<User, Post, _, _>("publish", |user, post| {
    let user_id = user.id;        // copy primitive
    let post_id = post.id;
    let admin   = user.is_admin;
    async move {
        // No `user` / `post` references here — only the captured copies.
        admin || check_can_publish(user_id, post_id).await
    }
});
```

Sync gates work transparently from the async path (`Gate::allows_async`
dispatches them without an `.await`), so a codebase can register
sync gates today and migrate individual abilities to async later
without changing call sites.

## Lock-poison posture

The `Gate` registry uses an `RwLock` internally. If the lock is ever
poisoned (a thread panicked while holding the write guard), the
registry **safe-denies** — every subsequent `authorize` call returns
`Unauthorized` rather than panicking. Registration calls log to
`tracing::error!` and continue. This matches the broader framework
policy: a poisoned lock never aborts the process.

## Rich decisions: `Response`, `inspect`, `raw`

A bare `bool` gate answers only allow/deny. For a denial that carries a
*message*, a machine *code*, or a non-403 HTTP *status*, register the gate
with `define_with` (or `define_async_with`) and return a `Response`:

```rust
use suprnova::authorization::Response;  // re-exported at the crate root as `GateResponse`

Gate::define_with::<User, Post>("update", |user, post| {
    if post.author_id == user.id {
        Response::allow()
    } else {
        Response::deny_with("You do not own this post.")
    }
});

// Hide a resource's existence rather than admit it exists:
Gate::define_with::<User, Secret>("view", |user, secret| {
    if user.can_see(secret) {
        Response::allow()
    } else {
        Response::deny_as_not_found()  // a 404, not a 403
    }
});
```

Inspect the full decision with `Gate::inspect` (sync) / `Gate::inspect_async`:

```rust
let decision = Gate::inspect("update", &user, &post);
decision.allowed();   // bool
decision.message();   // Option<&str>  — Some("You do not own this post.")
decision.status();    // Option<u16>   — None here; Some(404) after deny_as_not_found
```

`Response` constructors mirror Laravel: `allow()`, `deny()`,
`deny_with(msg)`, `deny_with_status(status, msg)`, `deny_as_not_found()`,
plus `with_message` / `with_code` / `with_status` / `as_not_found` builders.

### How a denial becomes an error

`Gate::authorize` collapses the decision through `Response::authorize()`:

| Decision | `authorize` result |
|---|---|
| allowed | `Ok(())` |
| bare `deny()` (no message/code/status) | `FrameworkError::Unauthorized` (403, `"This action is unauthorized."`) |
| rich denial (message and/or status set) | `FrameworkError::Domain { message, status_code }` |

So `deny_as_not_found()` surfaces as a 404, `deny_with_status(422, "…")` as a
422, and `deny_with("…")` as a 403 carrying your message. The `code` is
readable on the inspected `Response` but does **not** travel through
`authorize` — `FrameworkError` has no code field; read it from `inspect()` if
you need it.

### `raw`: "denied" vs "undefined"

`Gate::raw` (and `raw_async`) returns `Option<Response>`: `None` means *no
rule applied* — no `before` hook fired, no gate is registered, no `after`
hook filled in — as distinct from an explicit `Some(deny)`. `inspect`
normalizes that `None` to a default deny; `raw` preserves it for diagnostics
("is this action governed at all?").

## `before` / `after` hooks

`Gate::before` registers a check that runs *before* any gate; the first hook
to return `Some(decision)` short-circuits everything. The canonical use is a
global override:

```rust
// Administrators may do anything.
Gate::before::<User>(|user, _action| user.is_admin.then_some(true));
```

`Gate::after` runs *after* the gate. Following Laravel's `??=` semantic, an
after hook can only **fill in** an undecided result (no gate matched and no
before hook fired) — it can never override an allow/deny already produced.
Every after hook still runs, so it doubles as the audit-logging seam:

```rust
Gate::after::<User>(|user, action, decided| {
    audit_log(user.id, action, decided);   // observe every evaluation
    None                                    // record-only; don't change the result
});
```

Hooks are keyed by the **user type** `U`, not by resource — a hook fires for
every `(action, U, R)`. Put resource-specific logic in the gate. Hooks are
synchronous predicates and apply to the async evaluation path too; for async
authorization logic, use `define_async` / `define_async_with`.

## No `forUser`

Laravel's `Gate::forUser($user)->allows(...)` rebinds the gate's *implicit*
current-user resolver. suprnova's gate takes the user **explicitly** on every
call, so "check as a different user" is just `Gate::allows(action,
&other_user, &resource)`. There is no implicit resolver to rebind — the
explicit API is strictly more general, which makes `forUser` redundant rather
than missing.
