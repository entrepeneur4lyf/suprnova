# Eloquent Serialization

How Eloquent models turn into JSON. The chapter covers `to_array()` and
`to_json()`, the `hidden` / `visible` / `appends` filter pipeline, the
two terminal helpers `to_array_except` / `to_array_only`, the way
appends bridge accessors into the output, and the two divergences from
Laravel that catch people out: the serde-bypass footgun, and the fact
that eager-loaded relations do not auto-fold into the JSON body.

If you've read [Eloquent API](eloquent.md), most names here are
familiar — the attribute reference is in that chapter. This page is
where the *serialization contract* lives: which fields appear, in what
order the filters apply, and what produces a leak if you forget about
it.

## Table of contents

- [The contract](#the-contract)
- [`to_array` and `to_json`](#to_array-and-to_json)
- [Hiding fields — `hidden = [...]`](#hiding-fields--hidden--)
- [Whitelisting fields — `visible = [...]`](#whitelisting-fields--visible--)
- [Appending accessors — `appends = [...]`](#appending-accessors--appends--)
- [The filter pipeline order](#the-filter-pipeline-order)
- [Per-call filtering — `to_array_except` / `to_array_only`](#per-call-filtering--to_array_except--to_array_only)
- [Conditional hiding by viewer](#conditional-hiding-by-viewer)
- [The serde-bypass footgun](#the-serde-bypass-footgun)
- [Serializing collections](#serializing-collections)
- [Eager-loaded relations and serialization](#eager-loaded-relations-and-serialization)
- [What about JSON:API?](#what-about-jsonapi)
- [Where each piece lives](#where-each-piece-lives)
- [Next](#next)

## The contract

Every `#[suprnova::model]` struct gets two serialization methods from
the `Model` trait:

```rust
fn to_array(&self) -> serde_json::Value;
fn to_json(&self) -> String;
```

`to_array` produces a `serde_json::Value` for use in handler responses
and tests. `to_json` is a thin wrapper — `serde_json::to_string(&self
.to_array())` — so a single filter pipeline owns both shapes.

The output is a JSON object keyed by struct field name (or whatever
serde rename you've applied), filtered through three optional knobs
declared on `#[model(...)]`:

- `hidden = [...]` — column denylist
- `visible = [...]` — column whitelist (mutually exclusive with `hidden`)
- `appends = [...]` — accessor methods to inject under named keys

When the model declares none of these, the trait default body runs:
serialize `self` via `serde_json::to_value(self)`, strip two
framework-internal scratch fields (`__eager` and `__pivot` — see
[eager-loaded relations](#eager-loaded-relations-and-serialization)),
return the result. When the model declares any of them, the macro
emits an override that runs the [pipeline](#the-filter-pipeline-order).

## `to_array` and `to_json`

The minimum useful example — a row out the door as JSON:

```rust
use suprnova::{json_response, model, Model, Request, Response};
use chrono::{DateTime, Utc};

#[model(table = "users")]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn show(req: Request) -> Response {
    let id: i64 = req.param("id")?.parse()
        .map_err(|_| suprnova::FrameworkError::param_parse("id", "i64"))?;
    let user = User::find_or_fail(id).await?;
    Ok(json_response!(user.to_array()))
}
```

`json_response!` accepts any `serde_json::Value`; `user.to_array()`
produces one. The string-shaped equivalent is `user.to_json()` —
identical body, identical filters, just one extra `to_string`.

You can also reach for `serde_json::to_value(&user)` directly. **Don't
do that for anything user-facing.** It bypasses the filter pipeline
entirely — see [the serde-bypass footgun](#the-serde-bypass-footgun)
later in the chapter for why.

## Hiding fields — `hidden = [...]`

The denylist form. Every column except the listed ones serialises:

```rust
use chrono::{DateTime, Utc};
use suprnova::{model, Model};

#[model(
    table = "users",
    fillable = ["name", "email", "password"],
    hidden = ["password", "remember_token"],
)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub password: String,
    pub remember_token: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

The user-facing JSON for this model never contains `password` or
`remember_token`:

```json
{
    "id": 42,
    "name": "Alice",
    "email": "alice@example.com",
    "created_at": "2026-05-30T11:14:22Z",
    "updated_at": "2026-05-30T11:14:22Z"
}
```

`hidden` is the right tool when **most fields go to the wire** and you
need to subtract a small set of secrets, internal flags, or auth-only
data.

## Whitelisting fields — `visible = [...]`

The allowlist form. Only the listed columns serialise:

```rust
#[model(
    table = "users",
    visible = ["id", "name", "avatar_url"],
)]
pub struct PublicUserView { /* ... */ }
```

Useful for a model that exists specifically to be a thin public
projection (think Laravel's "Profile" / "PublicUser" types). `visible`
is also the right tool when the table holds dozens of internal columns
and only a few belong on the wire — listing the keep-set is shorter
than listing the strip-set.

`hidden` and `visible` are **mutually exclusive at compile time**. The
macro emits an error if you set both:

```text
error: cannot specify both `hidden` and `visible` on the same model
 --> src/models/user.rs:7:1
  |
7 | #[model(table = "users", hidden = ["x"], visible = ["y"])]
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

The two are policy opposites — pick the one whose intent matches the
shape of your model, not both.

## Appending accessors — `appends = [...]`

`appends` injects computed values into the JSON output. Each entry
names an `#[accessor]`-tagged method on the model; the macro calls it
during `to_array()` and stores the return value under the same key.

```rust
use suprnova::{accessor, model, Model};

#[model(
    table = "users",
    fillable = ["first_name", "last_name"],
    appends = ["full_name", "initials"],
)]
pub struct User {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl User {
    #[accessor]
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }

    #[accessor]
    pub fn initials(&self) -> String {
        let f = self.first_name.chars().next().unwrap_or(' ');
        let l = self.last_name.chars().next().unwrap_or(' ');
        format!("{f}{l}")
    }
}
```

The serialised user now carries both computed keys:

```json
{
    "id": 7,
    "first_name": "Alice",
    "last_name": "Pond",
    "created_at": "...",
    "updated_at": "...",
    "full_name": "Alice Pond",
    "initials": "AP"
}
```

The macro validates `appends` entries at compile time:

- Each name must parse as a Rust identifier (`"full-name"` fails — it's
  not a valid ident).
- If the named method doesn't exist on the model's `impl` block, the
  compiler points at the macro-generated dispatcher with a clear
  `no method named 'full_name' found` error.

Calling `user.full_name()` directly from Rust works exactly like any
other method — `appends` only controls the **JSON dispatch table**.
Accessors stay regular methods.

## The filter pipeline order

When a model declares any of `hidden`, `visible`, or `appends`, the
macro emits a `to_array` override that runs four steps in this order:

1. Serialise `self` to a `serde_json::Map` via `serde_json::to_value`.
2. Strip the framework-internal `__eager` and `__pivot` keys
   unconditionally (more on these in
   [the relations section](#eager-loaded-relations-and-serialization)).
3. Apply `visible` as a **whitelist** when non-empty: every key NOT in
   the list is removed.
4. Apply `hidden` as a **denylist**: every listed key that survived
   the whitelist is removed.
5. Inject `appends`: for each entry, call the registered accessor and
   insert its result under the entry's name.

### Why Suprnova diverges

Laravel runs the same `hidden` → `visible` → `appends` ordering. The
divergence is in step 5: in Suprnova, appends run **after** the hidden
denylist, and they always show up — even if their name is also listed
in `hidden`. The reasoning is the same as Laravel's: if you both
declare `$appends = ['full_name']` and `$hidden = ['full_name']`, the
intent is "compute it and ship it" — `appends` is the more specific
signal. The order matters when an accessor's key collides with a
column name (e.g. an accessor that overrides the stored `display_name`
column's value); the accessor wins on the wire.

## Per-call filtering — `to_array_except` / `to_array_only`

For one-off cases where the column declaration doesn't fit, two
terminal helpers run the full `to_array` pipeline then trim the result
by name:

```rust
use suprnova::{json_response, Model};

pub async fn admin_show(user: User) -> suprnova::Response {
    // strip a few extra fields for an admin endpoint that needs most
    // of the row but not these:
    Ok(json_response!(
        user.to_array_except(&["password_hash", "remember_token", "internal_notes"])
    ))
}

pub async fn directory_show(user: User) -> suprnova::Response {
    // public directory — only the columns we want to publish:
    Ok(json_response!(
        user.to_array_only(&["id", "name", "avatar_url"])
    ))
}
```

Both produce a `serde_json::Value` — they don't mutate `self` and they
don't change future serialisations of the same row. They run the
full `hidden` / `visible` / `appends` pipeline first, then apply their
own trim on top. `to_array_only` returns a *fresh* JSON object
containing only the named keys; `to_array_except` returns the full
object minus the named keys.

### Why Suprnova diverges

Laravel's `$user->makeHidden(['x'])` and `$user->makeVisible(['x'])`
**mutate** the model instance — every subsequent `toArray()` call,
including ones that happen when the model is nested inside a parent's
serialisation, sees the changed state. Suprnova's helpers are
**terminal**. They produce a `Value` and stop. If you need the change
to propagate, declare it on `#[model(hidden = [...])]` /
`#[model(visible = [...])]` so the *type* expresses the policy, not a
hidden mutation on the instance.

The Rust-shaped reason: an Eloquent struct in Suprnova is a plain Rust
struct with no runtime attribute bag. There's no place for an
instance-side visibility flag to live without adding ambient hidden
state, which is the kind of footgun the framework intentionally
avoids.

## Conditional hiding by viewer

The idiomatic pattern when visibility depends on the viewer is a
match at the call site, branching into the right per-call filter:

```rust
use suprnova::{json_response, Model, Request, Response};

pub async fn show(req: Request) -> Response {
    let id: i64 = req.param("id")?.parse()
        .map_err(|_| suprnova::FrameworkError::param_parse("id", "i64"))?;
    let user = User::find_or_fail(id).await?;
    let viewer = req.user();
    let viewing_self = viewer.as_ref().map(|v| v.id) == Some(user.id);

    let body = if viewing_self {
        user.to_array()
    } else {
        user.to_array_except(&["email", "phone", "stripe_customer_id"])
    };

    Ok(json_response!(body))
}
```

For more elaborate per-viewer shape — different attributes for admins,
trial users, paid users — the right tool is the **JSON:API resource
layer** with `Maybe<T>` / `MissingValue<T>` fields. See
[JSON:API resources](eloquent-resources.md#conditional-attributes--maybet--missingvaluet)
for the declarative form.

## The serde-bypass footgun

This is the single most important thing to know about Eloquent
serialization in Suprnova.

**The `hidden` / `visible` / `appends` filters only run through
`to_array()` and `to_json()`.** They are *not* enforced by the derived
`Serialize` impl. Returning the struct through any other serde path
bypasses the filters entirely.

That means **all of these leak `password`**:

```rust
// Direct serde — bypasses to_array, hidden has no effect:
let raw = serde_json::to_value(&user).unwrap();

// json_response! with a struct field — same:
Ok(json_response!({ "user": user }))

// Nested inside another serializable container — same:
#[derive(Serialize)]
struct EnvelopeWithUser { ok: bool, user: User }
let env = EnvelopeWithUser { ok: true, user };
Ok(json_response!(env))

// Returning a Vec<User> through serde — same:
Ok(json_response!(users))   // where users: Vec<User>
```

Only these go through the filter pipeline:

```rust
Ok(json_response!(user.to_array()))
Ok(json_response!(users_collection.to_array()))  // Collection<User>
Ok(json_response!(user.to_array_except(&["secret"])))
Ok(json_response!(user.to_array_only(&["id", "name"])))
```

### Why this happens

Serde's blanket `Serialize for Vec<T>` (and any other container) calls
`T::serialize` directly. Suprnova's filter pipeline lives in the
`Model::to_array` trait method, not in `Serialize`. The trait method
doesn't get invoked unless you call it.

The framework guards against the *internal* footgun (`__eager` /
`__pivot` scratch fields are marked `#[serde(skip)]` so they don't
leak through either path), but the macro deliberately does **not**
emit `#[serde(skip_serializing)]` on hidden fields — doing so would
break legitimate uses of serde with the inner SeaORM model where a
caller wants the full row (e.g. internal RPC, persistence layers,
diagnostics, tests).

### The rule

For any value that crosses the trust boundary back to a client, walk
through `to_array()` or one of its filtered cousins. The four-line
contract that buys you the safety:

| Want | Use | Result |
|---|---|---|
| Serialise one model | `user.to_array()` | Filtered JSON object |
| Serialise a collection | `collection.to_array()` | Filtered JSON array |
| Subtract a few fields | `user.to_array_except(&["x"])` | Filtered + subtracted |
| Keep only a few fields | `user.to_array_only(&["x"])` | Only listed keys |

A linter or PR-time review for `json_response!\({.*: [a-z_]+ ?})` and
`serde_json::to_value\(&\w+\)` on model values is a cheap way to keep
the rule. The framework's own tests for `Model` serialisation cover
both paths.

## Serializing collections

A `Collection<M>` — returned by `Builder::get()`, `Model::all()`, and
relation accessors — has its own `to_array()` and `to_json()` that
walk the underlying `Vec<M>` and call **per-row** `to_array()`. The
result is a JSON array of filtered objects:

```rust
use suprnova::{json_response, Model};

pub async fn list() -> suprnova::Response {
    let users = User::all().await?;
    Ok(json_response!(users.to_array()))
}
```

This is the only place to get the per-row filter on a multi-row
result. `serde_json::to_value(&users)` would emit a Vec via serde's
blanket impl and bypass the filters on every row at once — the
collection-level helper exists exactly to close that gap.

```rust
// The Collection<M> override:
pub fn to_array(&self) -> Value {
    Value::Array(self.0.iter().map(|m| m.to_array()).collect())
}
```

For a paginator, the wrapped data lives in `LengthAwarePaginator::data
/ CursorPaginator::data` and is a `Vec<M>` — call `.to_array()` on
each item before assembling the paginator response, or use the
[JSON:API paginated form](eloquent-resources.md#pagination) which
handles per-row filtering as part of the resource pipeline.

## Eager-loaded relations and serialization

This is the second divergence to internalise.

When you call `.with(["posts"])` on a builder, the framework loads the
posts and stores them in a per-row `EagerLoadCache` (the auto-injected
`__eager` field). The accessor for reading them — `user.posts_loaded()`
— pulls from that cache.

**The cache is `#[serde(skip)]` and `to_array()` strips it
unconditionally.** Eager-loaded relations do not auto-fold into the
JSON output. A `to_array()` on a user with eagerly-loaded posts looks
identical to a `to_array()` on a user without.

### Why Suprnova diverges

Laravel's `toArray()` walks `$model->getRelations()` and folds every
loaded relation into the output. PHP's array-shaped model bag makes
this natural — a relation is just another keyed entry on the model.

Rust's typed Eloquent structs don't have that bag. A `User` struct has
typed columns, not a heterogeneous map of "whatever relations were
loaded". Folding `posts` in would require either runtime field
injection on a typed struct (a serde-bypass mechanism), or a parallel
serialisation path that consults the cache after running the column
serialiser. Both options would couple every model's JSON shape to
which relations a particular caller eager-loaded — a contract that's
load-bearing in PHP because clients learn to depend on it, and a
contract Suprnova explicitly refuses to ship because it makes JSON
shape depend on caller-side query construction.

### The two ways to ship relation data

**1. Explicit accessor + appends.** Define a method that pulls from
`<rel>_loaded()`, register it in `appends`. The relation shows up
under whatever key you name. This works when the relation is *always*
eager-loaded on the read path:

```rust
use suprnova::{accessor, model};
use serde_json::Value;

#[model(
    table = "users",
    appends = ["posts"],
)]
pub struct User { /* ... */ }

impl User {
    #[accessor]
    pub fn posts(&self) -> Value {
        // posts_loaded() PANICS if .with(["posts"]) wasn't called on
        // the read path. The accessor MUST run after eager-loading.
        let posts = self.posts_loaded();
        serde_json::to_value(posts).unwrap_or(Value::Null)
    }
}

// Read path MUST eager-load:
let users = User::query()
    .with(["posts"])
    .get()
    .await?;
let body = users.to_array();   // each user's "posts" key is populated
```

The contract is loud: forget the `.with(["posts"])`, and the accessor
panics on the first row's `posts_loaded()` call (the eager cache
panics on read when the relation wasn't loaded, by design — a silent
empty array would hide the bug). For optional eager-load, use the
HasOne form which returns `Option<&T>` and gives you a `match`:

```rust
impl User {
    #[accessor]
    pub fn profile(&self) -> Value {
        match self.profile_loaded() {
            Some(profile) => serde_json::to_value(profile).unwrap_or(Value::Null),
            None => Value::Null,
        }
    }
}
```

**2. The JSON:API resource layer.** When the relation shape and
inclusion policy belong on the wire format rather than the model, use
a `#[derive(Data)] #[json_resource]` struct with
`#[data(allow_include)]` on the relationship field. Clients opt in via
`?include=posts.comments`, the framework walks the include tree, and
populates `included` with deduplicated resource objects. This is the
right answer when:

- Relation shape is a wire-format concern (sparse fieldsets, conditional
  inclusion, cross-link metadata).
- Different endpoints want different default inclusions.
- The same model appears under different envelopes (one endpoint ships
  `posts`, another ships `subscriptions`).

See [JSON:API resources](eloquent-resources.md#compound-documents--include-chains)
for the full pattern.

## What about JSON:API?

The `to_array()` pipeline and the `Resource` / `JsonApi` facade are
two layers, and they serve different jobs:

| Concern | `Model::to_array` | `Resource::single` / `JsonApi::single` |
|---|---|---|
| **Shape** | Flat object — column names map directly to keys | JSON:API envelope (`data`, `included`, `meta`, `links`, `jsonapi`) |
| **Per-attribute control** | `hidden` / `visible` / `appends` on `#[model]` | `#[data(input_only)]`, `Maybe<T>`, sparse fieldsets via `?fields[type]=` |
| **Relations** | Manual (accessor + appends, see above) | First-class via `#[data(allow_include)]` + `?include=` |
| **Pagination** | Wrap a `Vec<Value>` by hand | `Resource::paginated(p)` handles links + meta |
| **Errors** | Render through `FrameworkError` | `into_json_api_response()` produces JSON:API `errors` envelope |
| **When to reach for it** | Simple endpoints, internal tools, ad-hoc shapes | Public APIs, third-party consumers, JSON:API-aware clients |

`to_array()` is the lower layer — it's what gets called for most
internal handlers, admin pages, Inertia props (via serde), and tests.
The JSON:API layer composes on top: it doesn't replace `to_array`, it
adds an envelope around per-resource attribute / relationship logic
that's too rich to live on the model itself.

For typed Inertia props you almost always want the resource layer or a
dedicated `#[derive(Serialize)]` DTO with explicit fields rather than
piping the model through serde directly. Inertia returns get the same
serde-bypass treatment as everything else — the safe path is "build a
DTO, fill it from `to_array()`, return the DTO".

## Where each piece lives

| Concern | File |
|---|---|
| `Model::to_array` / `to_json` trait defaults | `framework/src/eloquent/model.rs` |
| `Model::to_array_except` / `to_array_only` | `framework/src/eloquent/model.rs` |
| `Model::__append_accessor` trait default | `framework/src/eloquent/model.rs` |
| Macro-emitted `to_array` override (filter pipeline) | `suprnova-macros/src/model/serialization.rs` |
| Macro-emitted `__append_accessor` dispatcher | `suprnova-macros/src/model/serialization.rs` |
| `Collection<M>::to_array` / `to_json` | `framework/src/eloquent/collection.rs` |
| `EagerLoadCache` (the `__eager` field) | `framework/src/eloquent/relations/eager_cache.rs` |
| `hidden` / `visible` / `appends` macro parsing | `suprnova-macros/src/model/parse.rs` |
| `#[accessor]` function-level macro | `suprnova-macros/src/lib.rs` |

## Next

- [Eloquent API](eloquent.md) — the full model surface, attribute
  reference, and where `#[accessor]` / `#[mutator]` are defined
- [JSON:API resources](eloquent-resources.md) — the declarative
  resource layer for richer per-viewer shapes, sparse fieldsets, and
  compound `?include=` documents
- [Validation](validation.md) — how request input becomes a typed
  struct before the model layer sees it
- [Responses](responses.md) — `HttpResponse` builders, headers, and
  cookies; the surface `json_response!` ultimately produces
- [Error Model](error-model.md) — how an error becomes a JSON body
  with the same `request_id` correlation as the success path
