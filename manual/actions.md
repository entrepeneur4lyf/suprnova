# Actions

An action in Suprnova is a struct with one job: hold a single piece of
business logic behind one method. It's the Rust analogue of Laravel's
single-action invokable controllers — `RegisterUser`, `PublishPost`,
`ChargeInvoice`. The action lives in `src/actions/`, carries the
`#[injectable]` attribute so the container can resolve it, and exposes
an `execute(...)` method that controllers (and jobs, and other actions)
call. There is no `#[action]` macro and no framework-side enforcement
of "one method" — the shape is a convention, and `#[injectable]` is the
machinery that makes the convention painless.

```rust
use suprnova::{injectable, FrameworkError};

#[injectable]
pub struct RegisterUserAction {
    // Inject dependencies as fields — see "Dependencies" below
}

impl RegisterUserAction {
    pub async fn execute(&self, email: &str) -> Result<String, FrameworkError> {
        tracing::info!(action = "RegisterUser", email, "executed");
        Ok(format!("registered: {email}"))
    }
}
```

Resolve it from a handler with `App::resolve::<RegisterUserAction>()?`
and you've split your domain logic away from the HTTP layer without
inventing a service-layer base class. That's the whole pattern.

## Generating an action

```bash
suprnova make:action RegisterUser
```

The CLI normalises the name to PascalCase, appends `Action` if the
suffix is missing, then snake-cases the filename. So:

| `make:action <Name>` | Struct name | File |
|---|---|---|
| `RegisterUser` | `RegisterUserAction` | `src/actions/register_user_action.rs` |
| `SendNotification` | `SendNotificationAction` | `src/actions/send_notification_action.rs` |
| `ProcessPayment` | `ProcessPaymentAction` | `src/actions/process_payment_action.rs` |
| `ChargeInvoiceAction` | `ChargeInvoiceAction` | `src/actions/charge_invoice_action.rs` |

The generator writes the file and adds a `pub mod register_user_action;`
line to `src/actions/mod.rs`. The emitted stub compiles immediately:

```rust
//! register_user_action action

use suprnova::{injectable, FrameworkError};

/// RegisterUserAction
///
/// Single-responsibility command resolved from the container. Inject any
/// dependencies as fields and the `#[injectable]` macro wires them at
/// resolve time.
#[injectable]
pub struct RegisterUserAction {
    // Add injected dependencies as fields here, e.g.
    // db: suprnova::DbConnection,
}

impl RegisterUserAction {
    /// Execute the action.
    pub async fn execute(&self) -> Result<String, FrameworkError> {
        Ok("RegisterUserAction executed".to_string())
    }
}
```

The signature — `async fn execute(&self) -> Result<_, FrameworkError>` —
is the production-safe shape: async, returning a `Result` that converts
through `?` straight into an `HttpResponse` at the call site. The body
is a placeholder; swap it for the real workflow.

## The `#[injectable]` attribute

`#[injectable]` is the only piece of framework machinery the action
pattern relies on. It expands into three things:

1. A `#[derive(Clone)]` on the struct (and `Default` when there are no
   `#[inject]` fields).
2. An `inventory::submit!` entry so boot can discover the type.
3. An auto-registration closure that `App::singleton_if_absent` runs
   once during `boot_services()`.

The macro's contract:

| Struct shape | Behaviour |
|---|---|
| Unit struct (`pub struct Foo;`) | Derives `Default + Clone`, registers `Default::default()` |
| Named fields, none `#[inject]` | Derives `Default + Clone`, registers `Default::default()` |
| Named fields with `#[inject]` | Derives `Clone` only; each `#[inject]` field is resolved from the container at boot, non-inject fields default |
| Tuple struct | Rejected at compile time — "use named fields instead" |

A resolved action is a clone of the stored singleton. The cost is one
`Clone` per `App::resolve::<Action>()?` call, which for a unit struct or
a struct of `Arc`-wrapped services is a handful of refcount bumps. Heavy
state belongs behind `Arc<dyn …>` services that the action injects, not
inside the action itself.

### `#[inject]` happens at boot, not per call

When the framework boots, `App::boot_services()` walks every
`#[injectable]` registration and runs them in a fixed-point retry loop.
Each entry tries to resolve its `#[inject]` fields from the container.
If a dependency hasn't been registered yet, the entry defers to the next
iteration. The loop runs until either every entry succeeds or no
progress is made — and on failure the framework returns a structured
error naming the unresolvable type or the cycle.

The practical consequence: **`App::resolve::<MyAction>()` clones the
already-constructed singleton**. It does not run `#[inject]` resolution
on every call. Anything injectable that an action depends on must itself
be registered before the action — either via its own `#[injectable]`
attribute, or by a manual `App::bind` / `App::singleton` in your
`bootstrap()` function. The retry loop handles inventory ordering for
you; it does not invent missing services.

## Using an action from a controller

The standard handler shape: resolve, execute, render.

```rust
use suprnova::{App, Request, Response, ResponseExt, json_response};

use crate::actions::register_user_action::RegisterUserAction;

pub async fn store(_req: Request) -> Response {
    let action = App::resolve::<RegisterUserAction>()?;
    let result = action.execute("alice@example.com").await?;

    json_response!({ "ok": true, "result": result }).status(201)
}
```

Both `?` points work because both error types convert into
`HttpResponse` via `From` impls — `App::resolve` returns
`Result<T, FrameworkError>` and the framework error converter handles
the rest. Missing service registration surfaces as a 500 with the
service name in the structured log, not a panic. See
[Error Model](error-model.md) for the full picture.

If you'd rather avoid the `?` on the resolve — for example in a path
that should hard-fail at boot time — `App::get::<RegisterUserAction>()`
returns `Option<T>` and you can `.expect("registered at boot")` to
fail loudly if you got the wiring wrong.

## Async actions that touch the database

This is the path most actions actually take — load or write through an
Eloquent model. Lift the body from your domain; the surface is the
same.

```rust
use suprnova::{attrs, injectable, FrameworkError, Model};

use crate::models::todos::Todo;

#[injectable]
pub struct CreateRandomTodoAction;

impl CreateRandomTodoAction {
    pub async fn execute(&self) -> Result<Todo, FrameworkError> {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            % 10000;

        Todo::create(attrs! {
            title: format!("Todo #{}", n),
            description: format!("created at {}", n),
            done: false,
        })
        .await
    }
}

#[injectable]
pub struct ListTodosAction;

impl ListTodosAction {
    pub async fn execute(&self) -> Result<Vec<Todo>, FrameworkError> {
        Ok(<Todo as suprnova::eloquent::Model>::all().await?.into_vec())
    }
}
```

`Todo::create(attrs!{...})` and `Todo::all()` come from the
`#[suprnova::model]` macro. See [Eloquent](eloquent.md) for the model
surface. Note that `Model::all()` returns a `Collection<Todo>` — the
example calls `.into_vec()` to hand the controller a plain `Vec`; you
can also return the `Collection` directly and let the serialiser render
it.

Wiring those into a controller:

```rust
use suprnova::{App, Request, Response, ResponseExt, json_response};

use crate::actions::todo_action::{CreateRandomTodoAction, ListTodosAction};

pub async fn create_random(_req: Request) -> Response {
    let action = App::resolve::<CreateRandomTodoAction>()?;
    let todo = action.execute().await?;
    json_response!({ "ok": true, "todo": todo }).status(201)
}

pub async fn list(_req: Request) -> Response {
    let action = App::resolve::<ListTodosAction>()?;
    let todos = action.execute().await?;
    json_response!({ "ok": true, "todos": todos })
}
```

Two `?` per handler; the controller stays a thin adapter between HTTP
and the domain.

## Dependencies via `#[inject]`

When an action needs collaborators — a mailer, a logger, a domain
service — declare them as fields and tag each with `#[inject]`:

```rust
use suprnova::{injectable, FrameworkError};

use crate::services::{MailerService, LoggerService};

#[injectable]
pub struct SendWelcomeEmailAction {
    #[inject]
    mailer: MailerService,
    #[inject]
    logger: LoggerService,
}

impl SendWelcomeEmailAction {
    pub async fn execute(&self, to: &str) -> Result<(), FrameworkError> {
        self.logger.info(&format!("welcome → {to}"));
        self.mailer.send_welcome(to).await
    }
}
```

Both `MailerService` and `LoggerService` must themselves be
container-registered before this action boots — either with their own
`#[injectable]` attribute, or by a `bootstrap()` call:

```rust
// In src/bootstrap.rs
App::singleton(MailerService::from_env()?);
App::singleton(LoggerService::default());
```

If either dependency is missing when boot runs the fixed-point loop,
boot returns an error naming the unresolved type and the framework
exits non-zero rather than starting with a half-wired container.

Non-`#[inject]` fields fall back to `Default::default()`, so you can
mix injected dependencies with plain state without writing a
constructor.

## When to use an action

The rule of thumb: an action exists when the same piece of work is (or
might be) triggered from more than one entry point. A registration flow
that runs from both an HTTP route and a queued job belongs in
`RegisterUserAction`. A one-off "render this index page" handler does
not need an action — keep it in the controller.

| Good fit | Example |
|---|---|
| Multi-step business operations | `RegisterUserAction`, `CheckoutAction` |
| Work shared between HTTP + queue | `IssueRefundAction` (dispatched both ways) |
| Logic worth testing without a request | `CalculateTotalsAction` |
| External integrations | `SendEmailAction`, `SyncInventoryAction` |
| Anything the controller would otherwise inline + duplicate | rule-of-three trigger |

Compared to a controller, an action is reusable, has no `Request`
binding, and is trivial to call from a test (`App::resolve` + `await`).
A controller stays an HTTP-aware boundary that knows how to translate
an action's result into a `Response`.

| Controller | Action |
|---|---|
| Handles one route | Reusable across routes, jobs, schedules |
| Knows about `Request` / `Response` | Knows about your domain types |
| Returns `Response` | Returns `Result<T, FrameworkError>` |
| Calls actions | Called by controllers (and others) |

## Actions, the bus, and queues

Actions are not the only place business logic can live — the
[Bus](bus.md) handles dispatched commands with typed outputs, and the
[Queue](queues.md) handles work that should run on a worker. Choose by
how the work is invoked:

| You want… | Reach for |
|---|---|
| Synchronous business logic, callable from a controller or a job | **Action** (`#[injectable]` + `execute`) |
| A typed command with a registered handler, callable via `Bus::dispatch` | [Bus](bus.md) |
| Durable, retried, off-task work | [Queue](queues.md) |

Mixing is fine: a `BusHandler` or a `Job` often just resolves an action
and calls its `execute`. The action holds the domain logic; the bus or
queue holds the dispatch metadata.

## File layout

What `make:action` emits, plus the room to group:

```
src/
├── actions/
│   ├── mod.rs                          // pub mod register_user_action;
│   ├── register_user_action.rs
│   ├── send_welcome_email_action.rs
│   └── billing/                        // group by domain when the dir grows
│       ├── mod.rs
│       ├── charge_invoice_action.rs
│       └── issue_refund_action.rs
├── controllers/
└── main.rs
```

Nothing in the framework requires this layout; the generator writes
into `src/actions/` because that's the convention. Move an action to
`src/billing/actions/` and it'll keep working — `#[injectable]` is
location-agnostic.

## Testing an action

Because an action is just a container-resolvable struct with an `async`
method, the test surface is `App::resolve` + `await`. The same
`TestDatabase` test fixture used elsewhere works here:

```rust
use suprnova::{describe, expect, test, App};
use suprnova::testing::TestDatabase;

use crate::actions::todo_action::ListTodosAction;
use crate::models::todos::Todo;

describe!("ListTodosAction", {
    test!("returns all todos", async fn(_db: TestDatabase) {
        Todo::create(suprnova::attrs! { title: "Test", description: "", done: false })
            .await
            .unwrap();

        let action = App::resolve::<ListTodosAction>().unwrap();
        let todos = action.execute().await.unwrap();

        expect!(todos).to_have_length(1);
    });
});
```

See [Testing](testing.md) for the full `describe!` / `test!` / `expect!`
surface and for `TestContainer::fake` when you want to inject a
fake-mailer or fake-gateway into an action under test.

## Why Suprnova diverges

Laravel single-action controllers — classes with a `__invoke` method
in `App\Actions\` — are constructed per request. The container
resolves the class, runs constructor injection, and the instance is
thrown away when the response leaves. PHP's process-per-request model
makes that essentially free.

Suprnova actions are container-resident singletons: built once at boot
with `#[inject]` fields resolved then, cloned out on every
`App::resolve`. The pattern fits Rust because cloning a struct of
`Arc`-wrapped services costs a few refcount bumps, while
constructing-and-discarding a struct on every request would force every
field through allocation. The Laravel-shaped convention — one struct,
one method, named for the operation — survives intact; the wiring under
it is shaped for Tokio.

The other intentional split: controllers stay free functions (see
[Controllers](controllers.md)), so the HTTP layer is a pure
request-to-response transform with no DI surface of its own.
Constructor-style injection happens at the `#[injectable]` boundary,
inside the action, where it belongs.

## Next

- [Controllers](controllers.md) — the HTTP-facing free functions that resolve and call actions
- [Service Container](container.md) — what `App::resolve`, `App::singleton`, and the three-layer lookup actually do
- [Bus](bus.md) — typed command dispatch when you want a registered handler instead of a resolved action
- [Testing](testing.md) — `App::resolve` + `TestContainer::fake` for hermetic action tests
- [Error Model](error-model.md) — how `?` on `App::resolve::<Action>()?` and `action.execute().await?` collapses into a clean response
