# suprnova-macros

Procedural macros for the [Suprnova](https://github.com/entrepeneur4lyf/suprnova)
framework — a Laravel-inspired web framework for Rust.

This crate is an implementation detail. **Do not depend on it
directly.** The macros are re-exported from `suprnova` itself, which
is what application code should import.

```rust,ignore
use suprnova::{handler, model, request, redirect};
```

## What's inside

- `#[handler]` — controller-method attribute
- `#[suprnova::model(...)]` — Eloquent-style model declaration
  (table, fillable, casts, relations, soft-deletes, timestamps)
- `#[request]` + `#[derive(FormRequest)]` — validated request bodies
- `#[derive(Data)]` — typed action/response payloads
- `#[derive(InertiaProps)]` — page-prop derivation with TS-emit
- `inertia_response!` / `redirect!` — compile-time-validated response macros
- `#[service]` + `#[injectable]` — DI container registration
- `#[domain_error]` — typed error variants
- `#[derive(Command)]` + `#[console::command]` — per-project console binary commands
- `#[observer(Model)]` — auto-registered Model observers
- `#[scopes(Model)]` — local query scopes
- `#[derive(Factory)]` — Persistable factory derivation
- `#[suprnova_test]` — async test scaffolding

See the [Suprnova manual](https://github.com/entrepeneur4lyf/suprnova/tree/main/manual)
for usage and the [framework docs](https://github.com/entrepeneur4lyf/suprnova/tree/main/framework/src)
for source.

## License

MIT
