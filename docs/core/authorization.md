---
title: Authorization
description: Gates and policies for authorizing actions in suprnova
icon: shield-check
---

Authentication answers _"who are you?"_ — authorization answers _"are you
allowed to do this?"_ suprnova ships a Laravel-shaped `Gate` facade plus a
`Policy` trait + `#[policy]` macro for resource-oriented wiring, with sync
and async variants of every check so the same surface works whether your
policy body needs a DB hit or just a struct-field comparison.

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
| `Gate::authorize(action, &user, &resource)` | `Result<(), FrameworkError>` | Returns `Unauthorized` (403) — short-circuits handler with `?` |
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

## The `Policy` trait

When you have a resource type with several abilities, write a `Policy`
once and let `#[policy]` register every method as a gate:

```rust
use suprnova::policy;

struct User { id: i64, is_admin: bool }
struct Post { id: i64, author_id: i64, is_public: bool }
struct PostPolicy;

#[policy(User, Post)]
impl PostPolicy {
    fn view_any(_user: &User, _post: &Post) -> bool {
        true  // anyone can list posts
    }
    fn view(user: &User, post: &Post) -> bool {
        post.is_public || post.author_id == user.id || user.is_admin
    }
    fn update(user: &User, post: &Post) -> bool {
        post.author_id == user.id || user.is_admin
    }
    fn delete(user: &User, post: &Post) -> bool {
        user.is_admin
    }
}
```

The macro generates one `inventory::submit!` per impl method, each
calling `Gate::define::<User, Post>(action, fn)`. `Server::serve` drains
the inventory via `init_policies()` at boot, so by the time the first
request arrives every action is registered. `init_policies()` is also
public and idempotent — call it manually in tests.

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

### Convention defaults

The `Policy<U>` trait ships seven default method bodies matching
Laravel's resource policy verbs:

| Method | Default | Rationale |
|---|---|---|
| `view_any(user)` | `true` | Listing is typically public |
| `view(&self, user)` | `true` | Reading is typically public |
| `create(user)` | `true` | Creation is typically open |
| `update(&self, user)` | `false` | Mutation requires an explicit decision |
| `delete(&self, user)` | `false` | Same |
| `restore(&self, user)` | `false` | Soft-delete restore — explicit |
| `force_delete(&self, user)` | `false` | Permanent destruction — most restrictive default |

Override the methods you care about; omitted methods take the default
and are NOT registered by `#[policy]` (so they remain at the
trait-default unless someone calls them directly).

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

## What's not (yet) shipped

The remaining Laravel-Gate surface is a follow-up commit:

- **`Response` type** with rich denial messages + HTTP status: lets
  `authorize` emit 404 / 422 in addition to 403 with a message body.
- **`Gate::inspect` / `Gate::raw`**: depend on `Response`.
- **`Gate::before` / `Gate::after` hooks**: super-admin override +
  audit-logging seams. Type-erased; the hook author downcasts to
  their concrete user type.
- **`Gate::forUser`**: scoped impersonation for "would this other
  user be allowed?" UIs.

These are deliberately deferred so this commit can stay atomic; see
`docs/parity/authorization.md` for the design notes.
