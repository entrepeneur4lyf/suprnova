# Data Objects

Suprnova's `#[derive(Data)]` lets you describe an inbound request shape, an outbound response shape, and a TypeScript export in **one struct**.

## Quick start

```rust
use suprnova::Data;
use suprnova::data::Field;
use validator::Validate;

#[derive(Data, Validate)]
pub struct UserDto {
    pub id: i64,

    #[validate(email)]
    pub email: String,

    pub name: String,

    #[data(input_only)]
    #[validate(length(min = 8))]
    pub password: String,

    #[data(output_only)]
    pub display_handle: String,

    pub bio: Field<String>,
}
```

`#[derive(Data)]` generates:
- `Serialize` (skipping `#[data(input_only)]` fields)
- `Deserialize` (rejecting `#[data(output_only)]` fields in the payload, defaulting them to `T::default()`)
- `FormRequest` with `authorize: true` by default — handlers can take the type directly as an extractor
- `IntoInertiaData` (the `Inertia::data(component, dto)` dispatch path)
- An `inventory::submit!` registration for any `#[data(allow_include)]` fields

Add `#[derive(Validate)]` separately so `#[validate(...)]` attributes stay visible at the field call site.

## Field attributes

| Attribute | Effect |
|---|---|
| `#[data(input_only)]` | Accepted on Deserialize, omitted from Serialize |
| `#[data(output_only)]` | Rejected on Deserialize (422), included in Serialize |
| `#[data(allow_include)]` | Field is `?include=`-eligible. **Default-deny**: any `?include=foo` request where `foo` isn't on the allowlist returns 400 |
| `#[data(lazy)]` | Field is a `Prop` resolved against the request's include-set; auto-registers as `allow_include` |
| `#[data(lazy(inertia))]` | Same as `lazy`, tagged for Inertia's partial-reload protocol |
| `#[data(lazy(deferred))]` | Tagged for Inertia's deferred-props protocol |
| `#[data(lazy(closure))]` | Always resolved on initial visit; lazy on partial reloads |
| `#[data(lazy(when_loaded))]` | Resolved only if the source entity has the relation preloaded |
| `#[data(from_route_param)]` | Field value comes from a path capture (e.g. `/users/{id}`). Default key = field name; pass `#[data(from_route_param("id"))]` to override |

## Struct attributes

| Attribute | Effect |
|---|---|
| `#[data(auto_lazy)]` | Every `Prop`-typed field is implicitly `#[data(lazy)]` |
| `#[data(custom_authorize)]` | Skip generating `FormRequest` so you can write your own `authorize` |

## `Field<T>` — Absent / Null / Value

For PATCH endpoints where "absent from payload" must be distinguished from "explicit null":

```rust
use suprnova::data::Field;

match dto.bio {
    Field::Absent  => { /* don't touch this column */ },
    Field::Null    => { /* clear the column */ },
    Field::Value(text) => { /* set to text */ },
}
```

`Field::Absent` (default) round-trips to omitted-from-JSON when paired with `#[serde(default, skip_serializing_if = "Field::is_absent")]` at the call site. Without `skip_serializing_if`, `Absent` serializes to JSON `null`.

For three-way DB upserts: `dto.bio.into_option_or_null() -> Option<Option<T>>` maps `Absent → None`, `Null → Some(None)`, `Value(v) → Some(Some(v))`. Use this when "don't touch" and "set to NULL" need to be distinct downstream.

> **Caveat:** `Field<Option<T>>` is lossy — `Value(None)` and `Null` both serialize as JSON `null` and deserialize back to `Null`. For nullable inner types, prefer a flat `Field<T>` and let `Null` carry the "clear it" signal.

## `?include=` query string

The `IncludeMiddleware` parses the request's query string into a per-request `RequestIncludeSet`:

- `?include=foo,bar` — resolve lazy fields `foo` and `bar`.
- `?include[]=foo&include[]=bar` — array form, same result.
- `?exclude=`, `?only=`, `?except=` — Laravel-Data API parity.

Composition with `X-Inertia-Partial-Data` (Inertia's partial-reload header): the include-set + per-DTO allowlist runs **first** for owner-tagged lazy fields, so a request for a disallowed field returns 400 even if partial-data would have filtered it out. Partial-data is applied **after** as a final "only" filter on the resolved props.

Register `IncludeMiddleware` globally — typically between session and authorization in the middleware stack:

```text
SessionMiddleware → IncludeMiddleware → AuthMiddleware → handlers
```

## Generic structs

```rust
use serde::{Serialize, Deserialize};

#[derive(suprnova::Data)]
pub struct Paginated<T>
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    pub items: Vec<T>,
    pub total: usize,

    #[data(allow_include)]
    pub meta: Option<serde_json::Value>,
}
```

The TypeScript extractor emits `export interface Paginated<T>` so frontend code can reuse the generic across instantiations.

The `?include=` allowlist is keyed on the bare struct name (`Paginated`), not on instantiations. `Paginated<UserDto>` and `Paginated<ArticleDto>` share the same allowlist — `allow_include` names a field, and field names don't depend on type parameters.

Note: `FormRequest` is suppressed for generic structs because its trait bounds (`DeserializeOwned + Validate + Send`) can't be verified without knowing concrete type params. Provide your own impl if you need to extract a generic Data struct from a request.

## Route-parameter field injection

```rust
use suprnova::Data;
use validator::Validate;

#[derive(Data, Validate)]
pub struct UpdateUser {
    #[data(from_route_param("id"))]
    pub id: i64,

    #[validate(length(min = 1))]
    pub name: String,
}
```

For `PATCH /users/{id}` with body `{"name": "Ada"}`, the route-captured `id` is merged into the validated payload. **The path always wins over a body-supplied value** (prevents IDOR via body-tampering).

Bare `#[data(from_route_param)]` defaults to the field name. The macro picks a type-aware parser at compile time:

| Field type | Parser |
|---|---|
| `i8..i64`, `isize` | `parse_i64` |
| `u8..u64`, `usize` | `parse_u64` |
| `i128`, `u128` | `parse_i128` (string-passthrough; field's own `Deserialize` handles it) |
| `f32`, `f64` | `parse_f64` |
| `bool` | `parse_bool` (accepts `"true"` / `"false"`) |
| `String`, `Uuid`, `DateTime`, custom newtypes | `pass_string` (the field's own `Deserialize` does the work) |
| `Option<T>` of any of the above | Same parser as `T`; missing route param leaves the field absent |

## Lazy props

```rust
use suprnova::Data;
use suprnova::inertia::Prop;

#[derive(Data)]
#[data(auto_lazy)]
pub struct AlbumDto {
    pub id: i64,
    pub songs: Prop,    // auto-registered as ?include=songs
    pub artist: Prop,   // auto-registered as ?include=artist
}
```

Explicit per-field flavor:

```rust
#[derive(Data)]
pub struct AlbumDto {
    pub id: i64,

    #[data(lazy(inertia))]
    pub songs: Prop,

    #[data(lazy(deferred))]
    pub lyrics: Prop,

    #[data(lazy(closure))]
    pub artist: Prop,
}
```

Use `Inertia::data(component, dto)` to render — the derive generates an `IntoInertiaData` impl that consults the include-set and allowlist:

```rust
return Inertia::data("Album/Show", album_dto);
```

Note: lazy-bearing structs suppress `Serialize`, `Deserialize`, and `FormRequest` because `Prop` doesn't implement them. If a single endpoint needs both inbound parsing and lazy outbound, use two DTOs: one inbound (`#[derive(Data, Validate)]` plain) and one outbound (`#[derive(Data)]` with lazy fields).

## `when_loaded!` — relation-loaded conditional lazy

Mirrors Laravel-Data's `#[AutoWhenLoadedLazy]`. The user's `From<Entity>` impl decides whether the relation was preloaded:

```rust
use suprnova::data::{when_loaded, IsRelationLoaded};

impl From<&AlbumEntity> for AlbumDto {
    fn from(album: &AlbumEntity) -> Self {
        Self {
            id: album.id,
            songs: when_loaded!(album, "songs", || async {
                serde_json::json!(album.songs_relation()
                    .iter()
                    .map(SongDto::from)
                    .collect::<Vec<_>>())
            }),
            artist: Prop::eager(serde_json::json!(album.artist_name())),
            lyrics: Prop::lazy(|| async { /* ... */ }),
        }
    }
}
```

If the entity hasn't preloaded the named relation (per `IsRelationLoaded::is_relation_loaded`), `when_loaded!` returns `Prop::EagerNone` and the field is absent from the response.

SeaORM entities need a custom `IsRelationLoaded` impl that consults their loaded-relations state — there's no framework-supplied blanket impl because SeaORM's `ModelTrait` doesn't carry per-instance relation-loaded state (loaded relations live on query results, not the model struct itself).

## TypeScript export

`suprnova generate-types` emits TypeScript definitions for every `#[derive(Data)]` (and legacy `#[derive(InertiaProps)]`) struct. Behavior:

- `Field<T>` → `field?: T | null`
- `Prop` → `field?: T` (the lazy may-be-absent semantic; the `?` carries it, the type itself is plain)
- `#[data(input_only)]` → excluded from output type
- `#[data(output_only)]` → excluded from input type
- Generic struct → TypeScript generic interface (`export interface Paginated<T>`)
- When ANY field has `input_only` / `output_only` / `lazy`, two interfaces are emitted: `<Name>` (output) and `<Name>Input` (input)

Generated types never leak Rust-only types (`Prop<...>` won't appear in the output `.d.ts`).

## Scaffolding

```bash
suprnova make:inertia UserDto --data
```

Emits a `#[derive(Data, Validate)]` skeleton instead of the legacy `#[derive(InertiaProps)]` template.
