# JSON:API resources

Suprnova ships a first-class JSON:API resource layer. Mark a
`#[derive(Data)]` struct with `#[json_resource("type")]` and the
framework emits a fully-conformant `IntoJsonResource` impl alongside
the standard Inertia and serde impls — single envelopes, collections,
paginated collections, sparse fieldsets, compound `included`, and
multi-level `?include=a.b.c` chains all flow through the same code path.

The two facades — `Resource` and `JsonApi` — are the same type with
two names. Use whichever matches your house style; Laravel users will
recognise `JsonApi::single`, Rust users will reach for `Resource::single`.

## Defining a resource

```rust
use suprnova::{Data, Validate};

#[derive(Debug, Clone, Data, Validate)]
#[json_resource("users")]
pub struct UserResource {
    pub id: i64,
    pub email: String,

    // `input_only` keeps `password` available on the form-request side
    // but suppresses it from the API output.
    #[data(input_only)]
    pub password: String,

    // Marks a field as a *relationship*: it never lands in `attributes`,
    // it produces a JSON:API relationship object instead, and it is
    // eligible for `?include=`. The field type must implement
    // `IntoJsonResource` (directly, or via `Vec<T>` / `Option<T>`).
    #[data(allow_include)]
    pub posts: Vec<PostResource>,
}
```

The `id_field` keyword renames the field that supplies the JSON:API `id`:

```rust
#[derive(Data)]
#[json_resource("orders", id_field = "uuid")]
pub struct OrderResource {
    pub uuid: String,
    pub total_cents: i64,
}
```

## Rendering responses

Construct a pending response from a handler and call `.render().await`:

```rust
use suprnova::Resource;

#[handler]
async fn show_user(id: i64, db: DB) -> Result<HttpResponse, FrameworkError> {
    let user: UserResource = User::find(id).await?.into();
    Resource::single(user).render().await
}

#[handler]
async fn list_users(db: DB) -> Result<HttpResponse, FrameworkError> {
    let users: Vec<UserResource> = User::all().await?.into_iter().map(Into::into).collect();
    Resource::collection(users).render().await
}

#[handler]
async fn paginate_users(db: DB) -> Result<HttpResponse, FrameworkError> {
    let page = User::query().paginate(10, current_page()).await?;
    Resource::paginated(page).render().await
}
```

`JsonApi::single` / `JsonApi::collection` / `JsonApi::paginated` are
identical alias entry points if you prefer the Laravel spelling.

## Chainable mutators

`JsonApiResponse` is a pending object. Customise the envelope before
calling `.render().await`. Every mutator is `self` → `Self` so they
compose:

```rust
use suprnova::{Resource, JsonApiInfo};
use serde_json::json;

let info = JsonApiInfo::new()
    .with_version("1.1")
    .with_ext("https://jsonapi.org/ext/atomic")
    .with_meta("copyright", json!("2026 Acme Inc."));

Resource::single(user)
    .status(201)                                  // HTTP status override
    .with_meta("trace_id", json!("req-7"))        // top-level meta KV
    .with_link("self", "/api/users/1")            // top-level link
    .with_jsonapi(info)                           // top-level `jsonapi`
    .additional(json!({ "api_version": "2.0" }).as_object().unwrap().clone())
    .render()
    .await
```

| Mutator | Laravel analogue | Effect |
|---|---|---|
| `.status(code)` | `ResourceResponse::calculateStatus` | Overrides HTTP status. |
| `.created()` | `wasRecentlyCreated → 201` | Shorthand for `.status(201)`. |
| `.with_meta(k, v)` / `.meta(k, v)` | `with($request)` | Top-level `meta` KV. |
| `.with_meta_map(m)` | bulk `with($request)` | Merge a map into top-level `meta`. |
| `.with_link(rel, href)` / `.link(rel, href)` | `with($request)['links']` | Top-level `links` KV. |
| `.with_link_value(rel, v)` | link-object form | Top-level link as `{href, meta}`. |
| `.with_additional(k, v)` | `additional($data)` | Root-level key alongside `data`. |
| `.additional(map)` | `additional($data)` | Bulk additional keys. |
| `.with_jsonapi(info)` | `JsonApiResource::configure(...)` | Top-level `jsonapi` member. |

Canonical members (`data`, `included`, `links`, `meta`, `jsonapi`,
`errors`) are never overwritten by `.additional(...)`.

## Per-resource `links` and `meta`

Override the `IntoJsonResource::resource_links` and
`IntoJsonResource::resource_meta` defaults to attach links / metadata
to the *resource object*, not the document root:

```rust
use suprnova::resources::IntoJsonResource;
use serde_json::{Map, Value};

impl IntoJsonResource for MyHandRolledPost {
    // ...

    fn resource_links(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("self".into(), Value::String(format!("/api/posts/{}", self.id)));
        m
    }

    fn resource_meta(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("kind".into(), Value::String("blog".into()));
        m
    }
}
```

Both default to an empty `Map` for macro-derived resources, so the
JSON:API renderer omits the keys when not used. Override
`resource_top_level_meta` to lift per-resource metadata into the
envelope's top-level `meta` member.

## Conditional attributes — `Maybe<T>` / `MissingValue<T>`

Use `Maybe` to omit a field from the rendered `attributes` object based
on a runtime condition. This is the Suprnova analogue of Laravel's
`MissingValue` and the `when()` / `whenLoaded()` / `unless()` family.

```rust
use suprnova::{Maybe, MissingValue};

// Both names point at the same type.
let m1: Maybe<&str> = Maybe::present("email@example.com");
let m2: MissingValue<&str> = MissingValue::missing();
let m3 = Maybe::when(user.is_verified, &user.verified_at);
let m4 = Maybe::unless(user.is_admin, &user.public_handle);
let m5 = Maybe::when_with(expensive_check(), || compute_value()); // lazy
```

For macro-derived structs, declare a field as `Maybe<T>` and the
renderer drops it automatically when `Missing`. For hand-rolled
`resource_attributes`, use the `insert_maybe(map, key, maybe)` helper:

```rust
use suprnova::resources::{insert_maybe, Maybe};

fn resource_attributes(&self, _fs: Option<&[&str]>) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    insert_maybe(&mut map, "email", Maybe::present(&self.email));
    insert_maybe(
        &mut map,
        "phone",
        if self.show_phone { Maybe::present(&self.phone) } else { Maybe::missing() },
    );
    serde_json::Value::Object(map)
}
```

The renderer also calls `strip_missing_values(&mut value)` over the
entire attributes object, so `Maybe::Missing` values nested inside
arbitrary serde-derived structures are dropped recursively — useful
when a deeply-nested transformer wants to omit subfields.

## Sparse fieldsets

The framework's `IncludeMiddleware` parses
`?fields[type]=email,name`-style query parameters and binds them to a
task-local. The macro-emitted `resource_attributes` consults the
fieldset and only emits requested attributes. No handler-side work is
needed — install the middleware and the resource layer honours it
automatically.

```rust
// Request: GET /api/users/7?fields[users]=email
// Response: { "data": { "type": "users", "id": "7", "attributes": { "email": "alice@example.com" } } }
```

## Compound documents — `?include=` chains

Declare relationship fields with `#[data(allow_include)]`. The framework
builds an `IncludeTree` from `?include=author.posts.tags,comments`,
walks every node, and pushes fully-resolved resource objects into
`included` (deduplicated by `(type, id)`).

```rust
#[derive(Data)]
#[json_resource("posts")]
pub struct PostResource {
    pub id: i64,
    pub title: String,

    #[data(allow_include)]
    pub author: Option<AuthorResource>,

    #[data(allow_include)]
    pub tags: Vec<TagResource>,
}
```

A request that names an include path not on this resource's allowlist
gets a JSON:API 400 errors envelope — Suprnova is stricter than Laravel
here by design (it matches JSON:API spec §5.2.2 default-deny semantics).

## Pagination

`Resource::paginated(p)` works with any paginator implementing the
`Paginated<T>` trait — both `LengthAwarePaginator<T>` and
`CursorPaginator<T>` from `suprnova::pagination` ship this impl. The
renderer attaches `links.{self,first,prev,next,last}` and a
`meta.pagination` block automatically.

```rust
use suprnova::{LengthAwarePaginator, Resource};

let page = LengthAwarePaginator::new(items, total, per_page, current_page)
    .with_base_url("/api/users");
Resource::paginated(page).render().await
```

## Error envelopes

Every `FrameworkError` knows how to render itself as a JSON:API
`{"errors": [...]}` envelope via `into_json_api_response()`. The
helper is exposed because `FrameworkError` carries a status code, a
field-name source pointer (for `ValidationError`), and a request-id
correlation token under `meta.request_id`. 5xx responses are
sanitised: the raw message never reaches the client unless
`APP_DEBUG=true` is set in the active environment, in which case it
appears under `meta.debug_message`.

```rust
let response = FrameworkError::validation("email", "email is invalid")
    .into_json_api_response();
// {
//   "errors": [{
//     "status": "422",
//     "title": "Validation failed",
//     "detail": "email is invalid",
//     "source": { "pointer": "/data/attributes/email" },
//     "meta": { "request_id": "..." }
//   }]
// }
```

## Surfaces summary

| Suprnova surface | Laravel 13 equivalent |
|---|---|
| `Resource` / `JsonApi` facades | `JsonResource::make`, `JsonApiResource` |
| `JsonApiResponse` | `ResourceResponse`, `JsonApiResource::toResponse` |
| `JsonApiBuilder` | (internal builder for `ResourceResponse`) |
| `IntoJsonResource` trait | `JsonResource::toArray`, `toAttributes`, `toRelationships`, `toLinks`, `toMeta`, `with` |
| `RelationshipValue` / `ResourceIdentifier` | array shape inside `toRelationships` |
| `IncludeTree` | parsed `?include=` from `JsonApiRequest` |
| `RequestFieldsetSet` | parsed `?fields[type]=` from `JsonApiRequest` |
| `Maybe<T>` / `MissingValue<T>` | `MissingValue` + `whenLoaded` / `when` / `unless` |
| `JsonApiInfo` | `JsonApiResource::$jsonApiInformation` |
| `JsonApiResponse::status(code)` / `.created()` | `ResourceResponse::calculateStatus` |
| `JsonApiResponse::additional(map)` / `.with_additional(k, v)` | `JsonResource::additional($data)` |
| `JsonApiResponse::with_meta(k, v)` / `.meta(k, v)` | `JsonResource::with($request)['meta']` |
| `JsonApiResponse::with_link(rel, href)` / `.link(rel, href)` | `JsonResource::with($request)['links']` |
| `JsonApiResponse::with_jsonapi(info)` | `JsonApiResource::configure(...)` |
| `current_fieldset()` / `scope_fieldset(...)` | task-local fieldset, set by `IncludeMiddleware` |
| `IncludeResolutionError` → 400 envelope | strict-mode `?include=` parser |

Top-level re-exports under `suprnova::`: `Resource`, `JsonApi`,
`JsonApiResponse`, `JsonApiBuilder`, `JsonApiInfo`, `IntoJsonResource`,
`RelationshipValue`, `ResourceIdentifier`, `IncludeTree`,
`RequestFieldsetSet`, `Maybe`, `MissingValue`, `insert_maybe`,
`strip_missing_values`, `AsRelationshipValue`, `PushIncluded`,
`IncludeResolutionError`, `current_fieldset`, `scope_fieldset`.
