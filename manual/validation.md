# Validation

Suprnova validates request input on two complementary tracks:

1. **Derive validation** — `#[validate(...)]` attributes on a `FormRequest`
   struct, run automatically by `extract()`. This is the everyday path and
   is covered in [Requests](requests.md). It handles per-field
   rules (`email`, `length`, `range`, …) declaratively.
2. **Rule objects + the `validate!` macro** — plain values implementing
   [`Rule`](#rule-objects) / `ContextualRule` / `AsyncRule`, composed
   imperatively. Reach for these when you need cross-field logic, rules
   that touch the database, or rules you want to store and pass around.

The two tracks accumulate into the same
[`ValidationErrors`](errors.md) bag and render the same
Laravel/Inertia `{ "message", "errors": { field: [...] } }` shape (HTTP
422).

## Rule objects

A rule is a value implementing one of three traits:

| Trait | Shape | Use |
|-------|-------|-----|
| `Rule` | `passes(&self, value: &str)` | pure check on one value |
| `ContextualRule` | `passes(&self, value, ctx)` | check that reads sibling fields |
| `AsyncRule` | `async passes(&self, value)` | check that `.await`s (DB, HTTP) |

Built-in `Rule`s: `Required`, `Email`, `Min`, `Max`, `Between`, `In`,
`NotIn`, `Integer`, `Numeric`, `Boolean`, `Alpha`, `AlphaNum`, `Url`,
`HttpUrl`, `Uuid`. Built-in `ContextualRule`s: `RequiredIf`,
`RequiredWith`, `RequiredUnless`, `Same`, `Different`, `Confirmed`.
Built-in `AsyncRule`: [`Unique`](#the-unique-rule).

```rust
use suprnova::{Rule, rules::Email};

Email.passes("user@example.com")?; // Ok(())
```

> **Note:** `Numeric` accepts a **finite** number — `NaN`, `inf`, and magnitudes that
> overflow to infinity are rejected, even though Rust's parser would accept
> the strings. Use `HttpUrl` (not `Url`) for callback/webhook/avatar inputs:
> `Url` parses any scheme `url::Url` accepts (`file:`, `javascript:`, custom
> URIs), while `HttpUrl` requires `http`/`https`.

## The `validate!` macro

`validate!` runs a chain of rules over the fields of a struct, accumulating
every failure into one `ValidationErrors`. It's the idiomatic home for the
synchronous cross-field hook, [`after_validation`](#cross-field-hooks).

```rust
use suprnova::{validate, ValidationErrors, rules::{Required, Email, Min, Max, RequiredIf}};

fn after_validation(&self) -> Result<(), ValidationErrors> {
    // Contextual rules read sibling values from a `FormContext` you build
    // — a map of field name to its string value.
    let mut ctx = std::collections::HashMap::new();
    ctx.insert("billing_type".to_string(), self.billing_type.clone());
    validate! { self =>
        email       => Required, Email;          // required-shape row
        bio         ?: Min(10), Max(500);        // optional: validate only if Some
        card_number ?=> RequiredIf {             // conditional-presence (see below)
            other: "billing_type",
            value: "card",
        } => with ctx;
    }
}
```

Each row is one of three shapes:

- **`field => Rule1, Rule2;`** — required-shape. Rules run on `&self.field`
  directly (for `String`, `i64`, or anything that derefs to the rule's
  expected borrow).
- **`field ?: Rule1, Rule2;`** — optional. The field is `Option<T>`; rules
  run only when it is `Some`, and are **skipped entirely on `None`**. This
  is Laravel's "if present, validate" (`sometimes`) semantics.
- **`field ?=> Rule1, Rule2;`** — conditional-presence. Also for an
  `Option<String>` field, but rules run **even when `None`** (absence is
  treated as the empty string). This is the row for presence-conditional
  rules like `RequiredIf` that must be able to *fail an absent field* —
  the case `?:` cannot express because it skips on `None`.

A contextual rule is followed by `=> with $ctx` (an
`&HashMap<String, String>` of sibling values). The macro is **synchronous**
— for async rules use the [hook](#async-rules-in-requests) below.

> **Warning:** A common trap: writing `card_number ?: RequiredIf {...} => with ctx;`. On
> a `?:` row, `None` skips all rules, so `RequiredIf` can never fail an
> absent field. Use `?=>` for any rule that must fire on absence.

## Cross-field hooks

`FormRequest` runs two cross-field hooks after the derived per-field rules,
both in the normal and Precognition flows. `extract()` runs the stages in
order — derived `validate()`, then `after_validation`, then
`after_validation_async` — and **bails at the first failing stage**.

```rust
use suprnova::{FormRequest, ValidationErrors};
use serde::Deserialize;
use validator::Validate;

#[derive(Deserialize, Validate)]
pub struct UpdatePassword {
    #[validate(length(min = 8))]
    pub new_password: String,
    pub confirmation: String,
}

impl FormRequest for UpdatePassword {
    fn after_validation(&self) -> Result<(), ValidationErrors> {
        let mut errs = ValidationErrors::new();
        if self.new_password != self.confirmation {
            errs.add("confirmation", "passwords do not match");
        }
        errs.into_result()
    }
}
```

> **Note:** Override hooks need a hand-written `impl FormRequest` — the `#[request]`
> attribute and `#[derive(FormRequest)]` generate their own (empty) impl, so
> they're for the common no-override case only.

### Async rules in requests

The `validate!` macro can't weave in `.await`, so database-backed rules run
in `after_validation_async` — the final validation stage, which `extract()`
calls automatically. This is where [`Unique`](#the-unique-rule) and any
custom `AsyncRule` participate in automatic request validation; no
per-handler plumbing required.

```rust
use suprnova::{FormRequest, ValidationErrors, Unique, async_trait};
use serde::Deserialize;
use validator::Validate;

#[derive(Deserialize, Validate)]
pub struct CreateUser {
    #[validate(email)]
    pub email: String,
}

#[async_trait]
impl FormRequest for CreateUser {
    async fn after_validation_async(&self) -> Result<(), ValidationErrors> {
        let mut errs = ValidationErrors::new();
        Unique::new("users", "email")
            .check_async(&self.email, &mut errs, "email")
            .await;
        errs.into_result()
    }
}
```

Because the async stage runs only after the synchronous stages pass, a
malformed value (a syntactically invalid email) never reaches the database
`Unique` query.

## The `Unique` rule

`Unique` checks that a value does not already exist in a table. Build it
with `Unique::new(table, column)` and refine with the fluent API:

```rust
use suprnova::Unique;

// email must be unique, ignoring the row currently being edited
Unique::new("users", "email").ignore(current_user_id)

// email unique *per tenant*, compared case-insensitively
Unique::new("users", "email")
    .where_eq("tenant_id", tenant_id)
    .case_insensitive()
```

| Builder method | Effect |
|----------------|--------|
| `.ignore(id)` | exclude the row whose `id` equals `id` (edit-self case) |
| `.ignore_with_column(col, id)` | exclude on a non-`id` key column |
| `.where_eq(col, value)` | scope the check to rows where `col = value`; multiple calls AND together |
| `.case_insensitive()` | compare with `LOWER(col) = LOWER(?)` |

Table, column, the exclusion key, and every `where_eq` column are validated
against an identifier allowlist before they reach the SQL string; the value
under test and all scope values are bound parameters.

### Unique is advisory — the database constraint is the guarantee

`Unique` runs a `SELECT COUNT(*)` **before** the write, so it carries an
unavoidable time-of-check/time-of-use race: two concurrent requests can
both pass the check and then both insert. Laravel's `unique` rule has the
identical property. The **only** real guarantee is a `UNIQUE` constraint
(or unique index) on the column in your migration.

Use the three together:

1. **The advisory rule** — a fast, friendly "that email is taken" message
   before submit (and so Precognition can validate the field).
2. **The `UNIQUE` constraint** — the authoritative guard against the race.
3. **`FrameworkError::from_unique_violation`** — at the write site, map the
   constraint violation the loser of a race receives back to the same clean
   422, instead of leaking a 500:

```rust
use suprnova::FrameworkError;

// `users.email` has a UNIQUE constraint in the migration.
let user = new_user
    .insert(db)
    .await
    .map_err(|e| FrameworkError::from_unique_violation(
        "email",
        "That email address is already registered.",
        e,
    ))?;
```

`from_unique_violation` returns a 422 `Validation` error when the database
error is a unique-constraint violation, and passes any other error through
unchanged (MySQL, Postgres, and SQLite are all recognized).

## Async authorization

`FormRequest::authorize(&Request) -> bool` runs **before** the body is
parsed, so it can reject unauthorized requests without reading the payload.
It is synchronous by design: at that point the request still holds the
streaming body, so the hook cannot `.await`. Authorization that needs to
hit the database or an async policy belongs in one of these places, not in
`authorize`:

- **Middleware** — runs before `extract()`, is `async`, and short-circuits
  by returning `Err(response)` (see [Middleware](middleware.md)).
  The right place for "is this user allowed to reach this route at all".
- **The Gate** — call `Gate::allows_async` / `Gate::authorize_async` in the
  handler once you have the authenticated user and the resource (see
  [Authorization](authorization.md)).
- **`after_validation_async`** — for an authorization check that depends on
  the parsed request body, run it in the async hook alongside your other
  async rules.

## Design notes

- **Partial validation.** A `FormRequest` deserializes into a typed struct
  before validation runs, so the struct *is* the schema: a field that may
  be absent must be `Option<T>`. This is also what lets Precognition
  validate a partial payload — make the fields a draft can omit optional.
- **Rule messages.** Built-in rule messages are fixed English strings.
  There is no translation layer; wrap a rule (or add errors in
  `after_validation`) to localize.
- **`Min` / `Max` / `Between`** are string-length rules (counted in Unicode
  scalar values). For numeric bounds, validate with `#[validate(range(...))]`
  on the derive or a custom rule — the length rules are not value
  comparisons.

## Summary

| Task | API |
|------|-----|
| Per-field rules | `#[validate(...)]` on the `FormRequest` (see Requests) |
| Composed / cross-field rules | `validate! { self => ... }` |
| Optional "if present" | `field ?: Rule;` |
| Conditionally-required optional | `field ?=> Rule => with ctx;` |
| Async / DB-backed rule | `after_validation_async` + `AsyncRule::check_async` |
| Uniqueness | `Unique::new(t, c)` + `UNIQUE` constraint + `from_unique_violation` |
| Async authorization | middleware / `Gate::*_async` / `after_validation_async` |
