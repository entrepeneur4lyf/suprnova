# Code Generators

The `suprnova make:*` family scaffolds the conventional file for each
piece of a project — a controller, an action, a middleware, a console
command, a domain error, a scheduled task, an Inertia page or props
struct, a database migration — and wires the new module into its
parent `mod.rs` (and where needed, `src/lib.rs` and `cmd/main.rs`).
Reach for them when you'd otherwise be retyping the same boilerplate
+ `pub mod x;` import line, which is most of the time.

## make:controller

Scaffold a controller — a file in `src/controllers/` with a single
`#[handler]` async fn named `invoke`.

```bash
suprnova make:controller User
suprnova make:controller order_item
```

The name is normalised to `snake_case` for the file name and used
as-is for the `controller:` echo in the response. Only ASCII letters,
digits, and `_` are accepted — paths like `api/User` are rejected.

### Generated file

```rust
// src/controllers/user.rs
use suprnova::{handler, json_response, Request, Response};

#[handler]
pub async fn invoke(_req: Request) -> Response {
    json_response!({
        "controller": "User"
    })
}
```

### What it wires

1. Writes `src/controllers/<name>.rs` with the `#[handler]` fn.
2. Adds `pub mod <name>;` to `src/controllers/mod.rs` (creates the
   file if it didn't exist).
3. Prints a hint to add a route in `src/routes.rs`:
   `.get("/<name>", controllers::<name>::invoke)`.

See [Controllers](controllers.md) for the handler contract,
extractors, and the `routes!` macro.

---

## make:action

Scaffold a single-responsibility action — a container-resolvable
struct with an async `execute` method that returns a
`Result<String, FrameworkError>` so the skeleton compiles before you
fill in the body.

```bash
suprnova make:action CreateUser
suprnova make:action SendNotification
```

The name is PascalCased; an `Action` suffix is appended if missing,
and the file is the snake-cased struct name.

### Generated file

```rust
// src/actions/create_user_action.rs
use suprnova::{injectable, FrameworkError};

#[injectable]
pub struct CreateUserAction {
    // Add injected dependencies as fields here, e.g.
    // db: suprnova::DbConnection,
}

impl CreateUserAction {
    pub async fn execute(&self) -> Result<String, FrameworkError> {
        Ok("CreateUserAction executed".to_string())
    }
}
```

### What it wires

1. Writes `src/actions/<snake>.rs`.
2. Adds `pub mod <snake>;` to `src/actions/mod.rs`.
3. `#[injectable]` registers the action with the container at link
   time, so any controller can resolve it via `App::get::<CreateUserAction>()`
   and call `action.execute().await?`.

See [Actions](actions.md) for the resolve-and-invoke pattern and how
actions compose with the container.

---

## make:middleware

Scaffold a middleware — a unit struct that implements
`suprnova::Middleware`. The default body times the inner handler and
logs the inbound + outbound events with the per-request id, so it
runs end-to-end the first time.

```bash
suprnova make:middleware Auth
suprnova make:middleware RateLimit
```

The name is PascalCased; a `Middleware` suffix is appended if missing.
The file uses the snake-cased base name (without the suffix), e.g.
`Auth` → `src/middleware/auth.rs`, struct `AuthMiddleware`.

### Generated file

```rust
// src/middleware/auth.rs
use std::time::Instant;

use suprnova::{async_trait, current_request_id, Middleware, Next, Request, Response};

pub struct AuthMiddleware;

#[async_trait]
impl Middleware for AuthMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let method = request.method().to_string();
        let path = request.path().to_string();
        let request_id = current_request_id()
            .map(|id| id.as_str().to_string())
            .unwrap_or_default();
        let started_at = Instant::now();

        println!(
            "[AuthMiddleware] --> {} {} (request_id={})",
            method, path, request_id,
        );

        let response = next(request).await;

        println!(
            "[AuthMiddleware] <-- {} {} ({} ms, request_id={})",
            method, path, started_at.elapsed().as_millis(), request_id,
        );

        response
    }
}
```

### What it wires

1. Writes `src/middleware/<snake>.rs`.
2. Adds `mod <snake>;` + `pub use <snake>::<StructName>;` to
   `src/middleware/mod.rs` (creates it if needed).
3. Prints both the per-route shape
   (`.get("/path", handler).middleware(AuthMiddleware)`) and the
   global shape (`global_middleware!(middleware::AuthMiddleware)` in
   `bootstrap.rs`).

See [Middleware](middleware.md) for the full chain semantics,
ordering, and the global vs per-route distinction.

---

## make:command

Scaffold a console command — a `#[derive(clap::Parser, Command)]`
struct that the per-project `console` binary picks up via `inventory`
at link time. The default body is a `println!("…: not yet
implemented")` so the command runs immediately.

```bash
suprnova make:command CleanCache
suprnova make:command mail:send
suprnova make:command clean-cache
```

Naming follows three rules:

- Inputs containing `:` are used verbatim as the registered command
  name (Laravel namespace style: `db:seed`, `mail:send`).
- Otherwise the snake-cased fn name is kebabbed for the registered
  name (`CleanCache` → command `clean-cache`).
- The Rust file and struct are always snake-cased / PascalCased
  forms of the same identifier.

### Generated file

```rust
// src/commands/clean_cache.rs
use async_trait::async_trait;
use clap::Parser;
use suprnova::{Command, FrameworkError, TypedCommand};

#[derive(Parser, Command, Debug)]
#[console(name = "clean-cache", description = "TODO: describe what clean-cache does")]
pub struct CleanCache {
    // Add clap-derive args here.
}

#[async_trait]
impl TypedCommand for CleanCache {
    async fn run(self) -> Result<(), FrameworkError> {
        println!("clean-cache: not yet implemented");
        Ok(())
    }
}
```

### What it wires

1. Writes `src/commands/<snake>.rs`.
2. Adds `pub mod <snake>;` to `src/commands/mod.rs` (creates it if
   needed).
3. Warns loudly if `src/lib.rs` is missing `pub mod commands;` — the
   command won't link into the console binary without it.
4. Prints the run command: `cargo run --bin console -- clean-cache`.

See [Console](console.md) for the full typed-command surface, the
`#[command]` shorthand for argv-only handlers, and the per-project
console binary's role.

---

## make:error

Scaffold a domain error — a unit struct annotated with
`#[domain_error]` so it carries an HTTP status, a `Display` message,
and a `From<…> for FrameworkError` impl out of the box.

```bash
suprnova make:error UserNotFound
suprnova make:error PaymentFailed
```

The name is PascalCased for the struct and snake-cased for the file.
The default status is 500 and the message is the sentence-cased
struct name — change both attributes in the generated file to match
the situation.

### Generated file

```rust
// src/errors/user_not_found.rs
use suprnova::domain_error;

#[domain_error(status = 500, message = "User not found")]
pub struct UserNotFound;
```

Change `status = 500` to whatever fits — `404` for not-found,
`402` for payment-required, `403` for forbidden — and edit the
message string. For richer payloads, add named fields to the struct
and reference them in the message via interpolation in a hand-rolled
`Display` impl (drop the `#[domain_error]` macro at that point).

### What it wires

1. Writes `src/errors/<snake>.rs`.
2. Adds `pub mod <snake>;` to `src/errors/mod.rs` (creates it if
   needed).
3. Warns about declaring `mod errors;` in `src/lib.rs` if the
   `errors/` directory was created fresh.

### Using it

Inside a handler returning `Response`, lift the domain type to a
`FrameworkError` so `?` short-circuits cleanly:

```rust
use crate::errors::user_not_found::UserNotFound;
use suprnova::FrameworkError;

#[handler]
pub async fn show(req: Request) -> Response {
    let id = req.param("id")?;
    let user = find_user(id).await
        .ok_or_else(|| FrameworkError::from(UserNotFound))?;
    json_response!({ "user": user })
}
```

The [Errors](errors.md) chapter covers the full custom-error story,
including when to use `#[domain_error]` vs `AppError::bad_request(…)`
vs a hand-rolled `HttpError` impl.

---

## make:task

Scaffold a scheduled task — a unit struct that implements
`suprnova::Task` and prints structured start/finish lines so the
scaffold logs progress before you fill in the real body.

```bash
suprnova make:task CleanupLogs
suprnova make:task SendReminders
```

The name is PascalCased; a `Task` suffix is appended if missing.
The file is the snake-cased struct name, e.g. `CleanupLogs` →
`src/tasks/cleanup_logs_task.rs`.

### Generated file

```rust
// src/tasks/cleanup_logs_task.rs
use std::time::Instant;

use async_trait::async_trait;
use suprnova::{Task, TaskResult};

pub struct CleanupLogsTask;

impl CleanupLogsTask {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CleanupLogsTask {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Task for CleanupLogsTask {
    async fn handle(&self) -> TaskResult {
        let started_at = Instant::now();
        println!("[CleanupLogsTask] task started");

        // Replace this with the real job.

        println!(
            "[CleanupLogsTask] task finished in {} ms",
            started_at.elapsed().as_millis(),
        );
        Ok(())
    }
}
```

### What it wires

The first `make:task` invocation does heavier wiring than the other
generators — it creates the scheduler's surface in the project from
scratch:

1. Creates `src/tasks/` and `src/tasks/mod.rs` if missing.
2. Creates `src/schedule.rs` (the `register(schedule: &mut Schedule)`
   entrypoint) if missing.
3. Declares `pub mod schedule;` and `pub mod tasks;` in `src/lib.rs`.
4. Inserts `.schedule(<crate>::schedule::register)` into the
   `Application::new()` chain in `cmd/main.rs` or `src/main.rs`,
   immediately before `.run()`.
5. Writes `src/tasks/<snake>.rs` and adds it to `src/tasks/mod.rs`.

Subsequent invocations skip the steps that already ran.

### Registering the task

Open `src/schedule.rs` and add a registration call with the fluent
schedule API:

```rust
use suprnova::Schedule;
use crate::tasks::CleanupLogsTask;

pub fn register(schedule: &mut Schedule) {
    schedule.add(
        schedule.task(CleanupLogsTask::new())
            .daily()
            .at("03:00")
            .name("cleanup:logs")
            .description("Removes old log files daily"),
    );
}
```

Then run the scheduler:

```bash
suprnova schedule:work   # daemon — checks every minute
suprnova schedule:run    # one-shot — typically called by cron
suprnova schedule:list   # show every registered task
```

See [Scheduling](scheduling.md) for the full task surface (`hourly`,
`weekly`, `cron(...)`, `between`, `when`, `without_overlapping`,
timezone handling) and [CLI Scheduling](cli-scheduling.md) for the
run-as-cron vs run-as-daemon trade.

---

## make:inertia

Scaffold either an Inertia page component (default) or a typed Data
struct (`--data`), depending on the flag. The page generator detects
the frontend framework (Svelte 5, React 19, Vue 3.5) from `.env` and
emits the matching file extension.

### Page mode (default)

```bash
suprnova make:inertia About
suprnova make:inertia UserProfile
```

The name is PascalCased and the suffix `Page` is appended if missing,
so `About` → `AboutPage`. The file lands in `frontend/src/pages/`
with the per-frontend extension: `AboutPage.svelte` for Svelte,
`AboutPage.tsx` for React, `AboutPage.vue` for Vue.

Example (Svelte):

```svelte
<!-- frontend/src/pages/AboutPage.svelte -->
<div class="font-sans p-8 max-w-xl mx-auto">
  <h1 class="text-3xl font-bold">AboutPage</h1>
  <p class="mt-2">
    Edit <code class="bg-gray-100 px-1 rounded">frontend/src/pages/AboutPage.svelte</code> to get started.
  </p>
</div>
```

Render it from a controller:

```rust
inertia_response!(&req, "AboutPage", props)
```

See [Frontend Pages](frontend-pages.md) and
[Inertia Responses](frontend-inertia-responses.md) for the bridge
between controllers and pages, partial reloads, and shared props.

### Data struct mode (`--data`)

```bash
suprnova make:inertia UserProps --data
```

Emits a `#[derive(Data, Validate)]` struct in `app/src/props/`
(not `src/props/` — the `app/` prefix is hardcoded so the file lands
in the workspace's example/host app):

```rust
// app/src/props/user_props.rs
use suprnova::Data;
use validator::Validate;

#[derive(Data, Validate)]
pub struct UserProps {
    pub id: i64,
    // Add fields here.
    //
    // Available field attributes:
    //   #[data(input_only)]     — accepted on Deserialize, omitted from Serialize
    //   #[data(output_only)]    — rejected on Deserialize, included in Serialize
    //   #[data(allow_include)]  — registers as ?include=-eligible (default-deny)
    //
    // For PATCH endpoints, use suprnova::data::Field<T> to distinguish
    // absent from null. For lazy outbound fields, use suprnova::inertia::Prop<T>.
}
```

Use it in a controller to validate request bodies:

```rust
let dto: UserProps = req.validate_json().await?;
```

---

## make:migration

Scaffold a timestamped SeaORM migration file. Covered in detail in
[CLI Migrations](cli-migrations.md), which also walks the
`migrate` / `migrate:rollback` / `migrate:status` / `migrate:fresh` /
`db:sync` commands. The short form:

```bash
suprnova make:migration create_users_table
```

The migration name is preserved verbatim and prefixed with a
`YYYYMMDDHHMMSS_` stamp so files sort chronologically. The generated
file lands in `migrations/`.

See [Migrations](migrations.md) for the schema-builder surface and
[Database Testing](database-testing.md) for the `TestDatabase::fresh`
pattern that runs migrations against an isolated database per test.

---

## generate-types

Emit TypeScript interfaces from every Rust struct annotated with
`#[derive(InertiaProps)]`. The dev server runs this automatically; the
standalone command is for CI checks and one-shot regenerations.

```bash
suprnova generate-types [--output <PATH>] [--watch]
```

| Option | Default | Description |
|---|---|---|
| `-o, --output <PATH>` | `frontend/src/types/inertia-props.ts` | Output file path |
| `-w, --watch` | off | Watch source files and regenerate on change |

```bash
# One-shot
suprnova generate-types

# Watch mode (useful when you don't want to run the full dev server)
suprnova generate-types --watch

# Custom output path
suprnova generate-types --output frontend/src/types/props.ts
```

A Rust shape on the left produces a TypeScript interface on the right:

```rust
#[derive(InertiaProps)]
pub struct UserPageProps {
    pub user: User,
    pub posts: Vec<Post>,
}
```

```typescript
export interface UserPageProps {
    user: User;
    posts: Post[];
}
```

See [Frontend TypeScript Types](frontend-typescript-types.md) for the
full mapping table (enums, options, dates, nested structs) and the
override hooks.

---

### Why Suprnova diverges

Laravel's `php artisan make:*` drops a file in the right directory
and that's it — PSR-4 autoloading picks the new class up the next
time the framework boots. Rust has no equivalent. A file at
`src/foo/bar.rs` isn't compiled into the crate until `src/foo/mod.rs`
declares `pub mod bar;`, and the parent directory has to be wired up
the same way in `src/lib.rs`.

So every `suprnova make:*` generator does two things instead of one:
it writes the new file *and* edits the closest `mod.rs` (and, for
`make:task` and `make:command`, `src/lib.rs` and `cmd/main.rs` as
well). That's why every generator prints a `Created src/.../mod.rs`
or `Updated src/.../mod.rs` line — the wiring is part of the work,
not a follow-up step you remember on your own.

---

## Summary

| Command | Creates | Wires into |
|---|---|---|
| `make:controller <name>` | `src/controllers/<snake>.rs` | `controllers/mod.rs` |
| `make:action <Name>` | `src/actions/<snake>_action.rs` | `actions/mod.rs` |
| `make:middleware <Name>` | `src/middleware/<snake>.rs` | `middleware/mod.rs` |
| `make:command <name>` | `src/commands/<snake>.rs` | `commands/mod.rs` (+ warns about `lib.rs`) |
| `make:error <Name>` | `src/errors/<snake>.rs` | `errors/mod.rs` |
| `make:task <Name>` | `src/tasks/<snake>_task.rs` | `tasks/mod.rs`, `schedule.rs`, `lib.rs`, `main.rs` |
| `make:inertia <Name>` | `frontend/src/pages/<Name>Page.<ext>` | (no module wiring) |
| `make:inertia <Name> --data` | `app/src/props/<snake>.rs` | (no module wiring) |
| `make:migration <name>` | `migrations/YYYYMMDDHHMMSS_<name>.rs` | (no module wiring) |
| `generate-types` | `frontend/src/types/inertia-props.ts` | n/a |

## Next

- [CLI Overview](cli.md) — the full subcommand table
- [Console](console.md) — the per-project console binary that
  `make:command` feeds into
- [Controllers](controllers.md) — the handler contract `make:controller`
  scaffolds
- [Scheduling](scheduling.md) — the fluent schedule API used to
  register tasks generated by `make:task`
- [CLI Migrations](cli-migrations.md) — the migrate / db:sync
  commands that pair with `make:migration`
