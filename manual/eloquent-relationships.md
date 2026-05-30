# Eloquent Relationships

[Eloquent](eloquent.md) covers the day-to-day relationship surface —
declaration syntax, the option table, basic per-kind chaining. This
chapter is the relationship-specific deep dive: how a `user.posts()`
call actually resolves to SQL, how the eager loader avoids N+1, how
the existence engine (`has` / `where_has` / `where_belongs_to`) renders
correlated `EXISTS` subqueries, how polymorphism survives Rust's lack
of late static binding, and what falls out of the type system when
all eleven relation kinds have to coexist on one trait.

If you're new to Eloquent on Suprnova, read
[Eloquent](eloquent.md#relationships) first — that page teaches the
declaration syntax. This page assumes you have a model with a
`relations = { ... }` block already and want to understand what's
underneath.

## The eleven relation kinds

Every relation kind in [`RelationKind`][relations] is one of:

| Kind                  | Side       | Cardinality | Across families | Pivot |
|-----------------------|------------|-------------|-----------------|-------|
| `HasOne<R>`           | parent     | one         | no              | —     |
| `HasMany<R>`          | parent     | many        | no              | —     |
| `BelongsTo<R>`        | child      | one         | no              | —     |
| `BelongsToMany<R, P>` | either     | many        | no              | yes   |
| `HasOneThrough<B, R>` | parent     | one         | no              | —     |
| `HasManyThrough<B, R>`| parent     | many        | no              | —     |
| `MorphOne<R>`         | parent     | one         | yes             | —     |
| `MorphMany<R>`        | parent     | many        | yes             | —     |
| `MorphTo`             | child      | one         | yes (n targets) | —     |
| `MorphToMany<R, P>`   | parent     | many        | yes             | yes   |
| `MorphedByMany<R, P>` | m2m partner| many        | yes (inverse)   | yes   |

"Across families" means the related row's *type* varies — a `Comment`
might belong to a `Post` or a `Video`, not just one fixed parent table.
That's polymorphism, and Suprnova handles it via the [morph
registry](#the-morph-registry) plus a per-family enum.

[relations]: https://docs.rs/suprnova

### What the macro emits

When you write:

```rust
use suprnova::model;

#[model(table = "users", relations = {
    posts: HasMany<Post>,
})]
pub struct User {
    pub id: i64,
    pub name: String,
}
```

`#[suprnova::model]` expands into five things for `posts`:

1. **Relation method** — `fn posts(&self) -> HasMany<Self, Post>`. Returns
   a lazy wrapper carrying `self.id` plus FK metadata; no SQL runs yet.
2. **Loaded-accessor** — `fn posts_loaded(&self) -> &[Post]`. Reads from
   the eager cache after `User::with(["posts"])`. Empty slice when no
   eager load ran.
3. **Count-accessor** — `fn posts_count(&self) -> u64`. Reads from the
   same cache after `User::with_count(["posts"])`.
4. **Dispatcher arm** — match arm in the model's `__eager_load`
   inherent method. The eager loader looks up `"posts"` and runs the
   `IN`-query.
5. **Inventory entry** — one `inventory::submit!(RelationEntry { ... })`
   so the relation is enumerable at runtime (admin tooling, the
   existence engine, the morph dispatcher all walk this).

You never see (4) or (5). They power the rest of this chapter.

## Lazy resolution: how `user.posts()` becomes SQL

`user.posts()` returns a `HasMany<User, Post>` wrapper, not a query
result. The wrapper holds the parent's PK value plus the FK column
name, and a pre-filtered `Builder<Post>` with
`WHERE posts.user_id = ?` already applied. Nothing has touched the
database yet.

```rust
use suprnova::Direction;

// No SQL.
let posts_q = user.posts();

// SQL: SELECT * FROM posts WHERE user_id = ? ORDER BY id DESC LIMIT 5
let recent = user.posts()
    .order_by("id", Direction::Desc)
    .limit(5)
    .get()
    .await?;

// SQL: SELECT COUNT(*) FROM posts WHERE user_id = ?
let n = user.posts().count().await?;
```

The dual-API surface ([Eloquent → Naming note](eloquent.md#naming-note-dual-api))
is honoured on the wrapper: both `.filter("col", v)` and
`.db_where("col", v)` work, identically. The chainable surface on
`HasOne` / `HasMany` / `MorphOne` / `MorphMany` covers `filter` /
`db_where` / `order_by` / `latest` / `oldest` / `limit` / `take`.
Through and morph m2m relations expose only their terminal methods —
they go through hand-written SQL stitches, not a `Builder<R>`, so
they can't compose with the standard chain. See [Through
relations](#hasonethrough-and-hasmanythrough) and [Polymorphic
m2m](#morphtomany-and-morphedbymany) below.

### Soft deletes follow through

When the related type implements [`SoftDeletes`](eloquent.md#soft-deletes-flag),
the relation wrapper inherits its global scope. `user.posts().get()`
hides trashed posts the same way `Post::query().get()` does. Three
forwarders punch through:

```rust
let alive = user.posts().get().await?;                 // default: alive only
let all = user.posts().with_trashed().get().await?;    // alive + trashed
let dead = user.posts().only_trashed().get().await?;   // trashed only
```

`with_trashed` / `only_trashed` exist on `HasOne`, `HasMany`,
`MorphOne`, `MorphMany`, `BelongsToMany`, `MorphToMany`,
`MorphedByMany`, and `BelongsTo`. They are deliberately absent from
`HasOneThrough` and `HasManyThrough` — see the [Through soft-delete
gap](#through-soft-deletes-v1) below.

## One-to-one: `HasOne` and `BelongsTo`

`HasOne` is the parent saying "this child has a column pointing at me".
`BelongsTo` is the child saying "I have a column pointing at the
parent". Both run a single `WHERE fk = ? LIMIT 1` and return
`Option<R>`.

```rust
// HasOne — parent → child
let profile: Option<Profile> = user.profile().first().await?;

// BelongsTo — child → parent
let owner: Option<User> = profile.user().first().await?;
```

`BelongsTo` adds one Laravel-shaped affordance the others don't need:
`with_default`. When the child's FK is null OR the parent row was
deleted, `first()` returns the closure's stand-in rather than `None`:

```rust
#[model(table = "comments", relations = {
    author: BelongsTo<User> {
        with_default = || User { id: 0, name: "Guest".into(), .. },
    },
})]
pub struct Comment { /* ... */ }

// Always returns Some(User) — either the real author or the Guest stub.
let display: Option<User> = comment.author().first().await?;
```

The eager-load dispatcher honours the same fallback — lazy and eager
paths share the default behaviour, so template code that prints
`comment.author_loaded()[0].name` doesn't have to branch.

## One-to-many: `HasMany`

`HasMany` is the parent-side many-cardinality relation. The terminal
`.get()` returns a [`Collection<R>`](eloquent.md#collections) — the
Laravel-shaped wrapper around `Vec<R>` — so the model-aware surface
composes:

```rust
let titles = user.posts()
    .order_by("created_at", Direction::Desc)
    .limit(10)
    .get()
    .await?
    .pluck::<String>("title");
```

`latest()` and `oldest()` are sugar for
`order_by("created_at", Direction::Desc)` and `Asc` respectively —
they only resolve against models that declare a `created_at` column,
which the `#[suprnova::model]` macro auto-adds whenever timestamps are
on (the default).

## Many-to-many: `BelongsToMany<R, P>` and the first-class pivot

`BelongsToMany` is many-to-many through a join table. Suprnova's pivot
is itself a `#[suprnova::model]` struct with its own migrations, its
own accessors, its own events. That's the divergence — see [below](#why-suprnova-diverges-pivot-is-a-real-model).

```rust
#[model(table = "users", relations = {
    roles: BelongsToMany<Role, RoleUser> {
        with_pivot = ["assigned_at"],
        with_timestamps,
    },
})]
pub struct User { /* ... */ }

#[model(table = "role_user", primary_key = "id")]
pub struct RoleUser {
    pub id: i64,
    pub user_id: i64,
    pub role_id: i64,
    pub assigned_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
```

Mutators run against the pivot row:

```rust
use suprnova::attrs;

user.roles().attach(role.id).await?;
user.roles().attach_with(role.id, attrs! { assigned_at: now }).await?;
user.roles().detach(role.id).await?;
user.roles().sync([role_a.id, role_b.id, role_c.id]).await?;
```

`sync` reads the current pivot set, computes
`attach_set = ids - current` and `detach_set = current - ids`, and
runs the deltas inside a transaction. Duplicates in the input set
collapse by their JSON-string form so `sync([1, 1, 2])` does what you
mean.

Reading goes through the two-query strategy:

```rust
// Query 1: SELECT roles.*, role_user.* via INNER JOIN, scoped by user_id.
// Query 2: SELECT role_user.* for the same join, to stamp __pivot per row.
let roles = user.roles().get().await?;

// Each role carries the pivot context the macro made accessible:
for r in &roles {
    let pivot = r.pivot::<RoleUser>().expect("loaded via BelongsToMany");
    println!("{} assigned at {:?}", r.name, pivot.assigned_at);
}
```

### Why Suprnova diverges: pivot is a real model

Laravel's pivot is an opaque per-attribute bag (`$role->pivot->note`).
Suprnova requires you to declare the pivot struct because Rust's type
system needs the columns at compile time — and once you've paid for
that declaration, the pivot gets the same `#[suprnova::model]`
treatment as any other table: migrations, events, observers,
factories, soft-delete. `r.pivot::<RoleUser>()` returns a typed
reference; no string-keyed attribute lookups, no surprises at runtime
when a column is misspelled.

The cost is one extra struct per pivot table. The benefit is that the
pivot can carry behaviour — domain logic, validation rules, audit
columns — without escaping into raw SQL.

## `HasOneThrough` and `HasManyThrough`

Two-hop relations: `A → B → C` where `B` is an intermediate model whose
FK points at `A`, and `C` is the final target whose FK points at `B`.
Classic example: `Country` has many `User`s; `User` has many `Post`s;
`Country::posts()` jumps both hops in one SQL round trip.

```rust
#[model(table = "countries", relations = {
    posts: HasManyThrough<User, Post>,
})]
pub struct Country { /* ... */ }

// Single INNER JOIN: SELECT posts.* FROM posts
//   INNER JOIN users ON posts.user_id = users.id
//   WHERE users.country_id = ?
let posts: Collection<Post> = country.posts().get().await?;
```

`HasOneThrough` has the same shape but `.get()` returns
`Option<C>` (matching the one-cardinality semantics) and `.first()` is
its alias.

Through wrappers expose only their terminals — `get` / `first` / `count`
plus the key setters (`first_key` / `second_key` / `local_key` /
`second_local_key`). They do not flow through a `Builder<C>`, so they
can't chain `.filter(...)` or `.order_by(...)`. If you need to filter
across the join, fall back to two explicit relation hops.

### Through soft-deletes (v1)

Through relations use raw `INNER JOIN` SQL rather than the
`Builder<C>` pipeline, so the global soft-delete scope that
`C::query()` would install (`WHERE c.deleted_at IS NULL`) is **not**
applied. Trashed intermediates and trashed targets both participate
in the JOIN.

This diverges from Laravel, where `hasManyThrough` filters both `B`
and `C` by `deleted_at IS NULL` when the models declare `SoftDeletes`.
Until the fix lands, callers needing scoped Through reads should chain
the two relations explicitly:

```rust
// Instead of country.posts().get():
let users = country.users().get().await?;
let user_ids: Vec<i64> = users.iter().map(|u| u.id).collect();
let posts = Post::query().filter_in("user_id", user_ids).get().await?;
// Both User and Post soft-delete scopes apply.
```

## Polymorphic relations

A polymorphic FK is a column pair: `<name>_id` (the row's primary key)
plus `<name>_type` (a string identifying *which table* the id lives
in). One `Comment` row can point at a `Post` or a `Video` without
adding either a `post_id` or `video_id` column.

Suprnova ships four polymorphic kinds: `MorphOne`, `MorphMany`,
`MorphTo`, and the m2m pair `MorphToMany` / `MorphedByMany`. They all
share one piece of infrastructure: [the morph registry](#the-morph-registry).

### `MorphOne<R>` and `MorphMany<R>` — parent side

`MorphOne` and `MorphMany` mirror `HasOne` and `HasMany` but layer the
`<name>_type` discriminator on top. The inner builder is pre-filtered
with `WHERE <name>_id = ? AND <name>_type = ?`, so polymorphic
children pointing at *other* families never appear in the result.

```rust
#[model(table = "posts", morph_type = "post", relations = {
    comments: MorphMany<Comment> { name = "commentable" },
})]
pub struct Post { /* ... */ }

#[model(table = "videos", morph_type = "video", relations = {
    comments: MorphMany<Comment> { name = "commentable" },
})]
pub struct Video { /* ... */ }

let post_comments = post.comments().get().await?;     // only commentable_type = 'post'
let video_comments = video.comments().get().await?;   // only commentable_type = 'video'
```

`morph_type = "post"` is the string the parent registers in the
child's `commentable_type` column. Default is the snake-cased struct
name, but overriding is the right move for any model you're shipping —
table-renaming refactors shouldn't break the polymorphic key.

### `MorphTo` and the per-family enum

`MorphTo` lives on the morph-table side. The user declares the
*targets list* up front:

```rust
#[model(table = "comments", relations = {
    commentable: MorphTo { name = "commentable", targets = [Post, Video] },
})]
pub struct Comment {
    pub id: i64,
    pub commentable_id: i64,
    pub commentable_type: String,
    pub body: String,
}
```

The macro emits a per-family enum at the declaration site:

```rust
// Emitted by the macro — you don't write this.
pub enum CommentableMorph {
    Post(Post),
    Video(Video),
    Unknown(String, i64),     // fallback for unregistered <name>_type
}
```

And `comment.commentable()` returns a fetch helper whose `.get()`
resolves to the enum:

```rust
match comment.commentable().get().await? {
    CommentableMorph::Post(post) => println!("on post: {}", post.title),
    CommentableMorph::Video(video) => println!("on video: {}", video.url),
    CommentableMorph::Unknown(t, id) => {
        eprintln!("orphaned commentable_type={t} id={id}");
    }
}
```

### Why Suprnova diverges: per-family enum

Laravel's `morphTo` returns `mixed` — PHP's dynamic dispatch resolves
the method at runtime. Rust has no late static binding, so Suprnova
makes the family explicit. The benefits beat the typing cost:

- **Exhaustive `match`** — the compiler tells you when a new morph
  target lands and you forgot to handle it.
- **`Unknown(String, id)` is type-safe** — orphaned rows from a
  removed parent model class are surfaced as a variant, not panicked
  on.
- **The targets list documents the schema** — reading the `MorphTo`
  declaration tells you every type that can sit on the other end. No
  database query required to enumerate them.

### v1 restriction: `MorphTo` is `i64`-only

`MorphTo::morph_id` is hard-coded to `i64`. Polymorphic targets must
therefore use `i64` primary keys, and the morph table's `<name>_id`
column must also be `i64`. Models whose PK is `String` or
`Uuid`-via-string cannot be `MorphTo` targets in v1. v2 will
parameterise the morph ID type so the full PK lattice (`i64` /
`String` / `Uuid`) is accepted.

This is a polymorphic-inverse-only restriction. `MorphOne` /
`MorphMany` / `MorphToMany` / `MorphedByMany` work fine with any PK
shape — they read the parent's already-typed `id` directly.

### `MorphToMany` and `MorphedByMany`

Polymorphic many-to-many through a single pivot. One side is
"morphable" (`Post.tags()`, `Video.tags()` — both go through the same
`taggables` pivot). The other is the shared m2m partner (`Tag.posts()`,
`Tag.videos()` — same pivot, scanned the other way).

```rust
#[model(table = "tags", relations = {
    posts: MorphedByMany<Post, Taggable> {
        name = "taggable",
        target_morph_type = "post",
    },
    videos: MorphedByMany<Video, Taggable> {
        name = "taggable",
        target_morph_type = "video",
    },
})]
pub struct Tag { /* ... */ }

#[model(table = "posts", morph_type = "post", relations = {
    tags: MorphToMany<Tag, Taggable> { name = "taggable" },
})]
pub struct Post { /* ... */ }

#[model(table = "taggables", primary_key = "id", timestamps = false)]
pub struct Taggable {
    pub id: i64,
    pub tag_id: i64,
    pub taggable_id: i64,
    pub taggable_type: String,
}
```

`MorphToMany` is the mutating side — `attach` / `attach_with` / `detach`
/ `sync` all live there. `MorphedByMany` is read-only: each `tag.posts()`
call returns only `Post`-typed taggables, each `tag.videos()` returns
only `Video`-typed taggables, no mixing in one collection.

Mutate from the morphable side:

```rust
post.tags().attach(rust_tag.id).await?;
post.tags().sync([rust_tag.id, async_tag.id]).await?;
```

Read from either:

```rust
let tags_on_post: Collection<Tag> = post.tags().get().await?;
let posts_with_rust_tag: Collection<Post> = rust_tag.posts().get().await?;
```

## The morph registry

Every struct annotated `#[suprnova::model(morph_type = "...")]` emits
one [`MorphTypeEntry`][morph] via `inventory::submit!` at compile
time. The registry powers three things:

1. **Per-family enum dispatch** — `MorphTo.get()` reads the child row's
   `<name>_type` string and looks it up to find the right enum variant.
2. **`MorphedByMany` target filtering** — `target_morph_type = "post"`
   resolves through the registry to ensure the type string is real.
3. **Sanity checks** — `find_morph_type("post")` returns `None` if no
   model has registered with that string, distinguishing
   "deliberately unregistered" from "typo".

```rust
use suprnova::{morph_types, find_morph_type, find_morph_type_by_id};
use std::any::TypeId;

for entry in morph_types() {
    println!("{} -> {}", entry.morph_type, entry.type_name);
}

if let Some(e) = find_morph_type("post") {
    assert_eq!(e.table, "posts");
}

let by_id = find_morph_type_by_id(TypeId::of::<Post>());
```

[morph]: https://docs.rs/suprnova

Models without a `morph_type = "..."` attribute deliberately don't
register — the registry is opt-in. A non-polymorphic `User` model
contributes nothing to it, which is what makes
`find_morph_type("user")` returning `None` a useful signal.

## Querying by relation existence

`has` / `where_has` / `doesnt_have` / `where_relation` /
`where_belongs_to` form Suprnova's relation-existence engine. They all
render as correlated `EXISTS (...)` subqueries against the **parent's
own SELECT** — no JOIN, no duplicate parent rows, no GROUP BY.

```rust
// Users with at least one post.
let with_posts = User::query().has("posts").get().await?;

// Users with at least three posts.
let prolific = User::query().has_count("posts", ">=", 3).get().await?;

// Users with at least one PUBLISHED post.
let published_authors = User::query()
    .where_has::<Post, _>("posts", |q| q.filter("published", true))
    .get()
    .await?;

// Users with NO posts.
let empty_users = User::query().doesnt_have("posts").get().await?;

// Users with no DRAFT posts (they may still have published ones).
let clean = User::query()
    .where_doesnt_have::<Post, _>("posts", |q| q.filter("published", false))
    .get()
    .await?;

// Shortcut: where_has + single column == match.
let same = User::query()
    .where_relation("posts", "published", true)
    .get()
    .await?;

// where_belongs_to — direct FK = ? on THIS table (no EXISTS needed,
// since the FK is on the child row).
let mine = Post::query()
    .where_belongs_to("author", user.id)
    .get()
    .await?;
```

### How it works

The engine walks the relation inventory at query-build time. For each
named relation, it pulls the `RelationEntry` and renders the
appropriate SQL shape per kind:

- `HasOne` / `HasMany` / `MorphOne` / `MorphMany` →
  `EXISTS (SELECT 1 FROM child WHERE child.<fk> = parent.<pk>)`.
  Morph kinds add `AND child.<name>_type = '<parent_morph_type>'`.
- `BelongsTo` →
  `EXISTS (SELECT 1 FROM parent WHERE parent.<pk> = child.<fk>)`.
- `BelongsToMany` / `MorphToMany` → joins through the pivot:
  `EXISTS (SELECT 1 FROM pivot WHERE pivot.<parent_fk> = parent.<pk> ...)`.
- Through relations → joins through the intermediate.

The closure form (`where_has::<R, _>(rel, |q| ...)`) constructs an
inner `Builder<R>`; whatever WHERE terms that builder produces land
inside the subquery's body. Placeholder numbering is monotonic across
the whole statement, so the engine works correctly with `$1`-style
Postgres parameters.

`where_belongs_to` is the one exception that doesn't render an
EXISTS. The belongs-to FK lives on the parent's *own* row, so a
direct `WHERE child.<fk> = ?` is exactly the right SQL — no subquery
needed. If the relation name is unknown to the parent's inventory,
the engine emits `WHERE 1 = 0` so the query safely returns nothing.

### Why this beats LEFT JOIN

Laravel's older `has` / `whereHas` engine used to emit JOINs and
duplicate parent rows; the correlated EXISTS rewrite landed in Laravel
9. Suprnova ships EXISTS from day one. The advantages: no duplicates
in the result set, no GROUP BY workarounds for aggregates, no need
for `DISTINCT`, and the database's optimiser sees a real subquery
instead of a JOIN it can't push predicates through. For
`has_count(rel, ">=", n)` the engine renders
`(SELECT COUNT(*) FROM child WHERE ...) >= n` directly — one query, one
plan.

## Eager loading — `with`, `with_count`, `with_*` aggregates

The lazy `user.posts().get()` does one query per parent. That's N+1
when you have many users:

```rust
// Bad: 1 query for users + 100 queries for posts.
let users = User::query().limit(100).get().await?;
for u in &users {
    let posts = u.posts().get().await?;
    /* ... */
}
```

`with(["posts"])` collapses that to two queries total — regardless of
the parent count:

```rust
// Good: 1 query for users + 1 IN-query for all posts.
let users = User::query()
    .with(["posts"])
    .limit(100)
    .get()
    .await?;

for u in &users {
    for post in u.posts_loaded() {       // reads from cache, no SQL
        println!("{}: {}", u.name, post.title);
    }
}
```

Nested paths work too — dot-separated relation names recurse:

```rust
let users = User::query()
    .with(["posts.comments.author"])
    .get()
    .await?;
// 4 queries: users, posts IN users.id, comments IN posts.id, authors IN comments.user_id.
```

### `with_count` and aggregates

`with_count` adds a per-relation `COUNT(*) GROUP BY parent_fk` aggregate
loaded alongside the parents — one extra query per relation:

```rust
let users = User::query().with_count(["posts"]).get().await?;
for u in &users {
    println!("{} has {} posts", u.name, u.posts_count());
}
```

Four aggregate variants stack: `with_sum`, `with_avg`, `with_min`,
`with_max`. The cache key shape is `<rel>_<kind>_<col>` so stacking
multiple aggregates on the same relation doesn't collide:

```rust
let users = User::query()
    .with_count(["posts"])
    .with_sum(("posts", "views"))
    .with_avg(("posts", "views"))
    .get()
    .await?;

for u in &users {
    println!(
        "{}: {} posts, {} views total, {} avg",
        u.name,
        u.posts_count(),
        u.posts_sum_of("views").unwrap_or(0.0),
        u.posts_avg_of("views").unwrap_or(0.0),
    );
}
```

See [Eloquent → Eager loading → Cache layout](eloquent.md#cache-layout)
for the full storage contract.

### Constrained eager loads — `with_where`

`with_where` filters which child rows land in the eager cache without
losing parents that have no matching children:

```rust
use suprnova::Builder;

let users = User::query()
    .with_where(("posts", |q: Builder<Post>| q.filter("published", true)))
    .get()
    .await?;
// Each u.posts_loaded() contains only published posts.
// Users with zero published posts still appear in the result set —
// their posts_loaded() returns an empty slice.
```

`with_where` differs from `where_has` in intent: `where_has` filters
the parent set ("users who have at least one published post");
`with_where` filters the eager cache ("for all users, load only their
published posts"). Use both together when you want both effects.

### Loading on already-fetched collections

When you fetch a `Collection<M>` without an eager-load plan, you can
attach one after the fact:

```rust
let mut users = User::query().get().await?;

users.load(["posts"]).await?;                 // unconditional
users.load_missing(["posts.comments"]).await?; // skip what's already loaded
```

`load_missing` walks each parent's `__eager` cache and only fires the
IN-query for rows that haven't already loaded the relation. Useful in
loops where some parents got eager-loaded earlier in the request and
others didn't.

### Opting out — `without`

`without` removes named relations from the eager plan, useful when a
base scope adds defaults you don't want for this call:

```rust
let users = User::query()
    .with(["profile", "posts", "team"])
    .without(["team"])     // drops team from the plan
    .get()
    .await?;
```

## The escape hatch

When a relation doesn't fit any of the eleven kinds — recursive trees,
polymorphic-through-non-id keys, three-way pivots, anything bespoke —
hand-write the method. The macro doesn't prevent it; you just don't
get the loaded-accessor or the eager-load dispatcher arm for that
relation.

```rust
impl User {
    /// Custom: most-recent post regardless of FK shape.
    pub async fn latest_post(&self) -> Result<Option<Post>, FrameworkError> {
        Post::query()
            .filter("user_id", self.id)
            .latest()
            .first()
            .await
    }
}
```

The trade-off is explicit: hand-written methods don't appear in the
`relations()` inventory, the existence engine doesn't know about them,
and the eager loader can't include them in a plan. For one-offs that's
fine. For anything you'd want to `with(["..."])`, declare it as a
proper relation kind even if you have to use the macro options to bend
it into shape.

## Next

- [Eloquent](eloquent.md) — the day-to-day model surface; relation
  declaration syntax lives there.
- [Database](database.md) — connections, transactions, multi-driver,
  the lower layer everything sits on.
- [Migrations](migrations.md) — the schema side of the FK columns these
  relations need to exist.
- [Query Builder](eloquent.md#query-builder-dual-api) — the dual-API
  surface that relation wrappers forward into.
- [Eloquent Resources](eloquent-resources.md) — turning loaded
  relations into JSON:API payloads for the wire.
