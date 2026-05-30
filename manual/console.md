# Console

Each Suprnova project ships with a `console` binary — the runtime command dispatcher for everything that needs the app's compiled types: database seeders, custom one-shot tasks, queue management commands, anything you'd build with Laravel's `php artisan`. Commands are async fns annotated with `#[command]`; the framework collects them via `inventory` at link time, so adding a new command is a single file with no central registry to edit.

## Quick Start

The recommended shape uses `#[derive(clap::Parser, Command)]` for typed args:

```rust
use async_trait::async_trait;
use clap::Parser;
use suprnova::{Command, FrameworkError, TypedCommand};

#[derive(Parser, Command, Debug)]
#[console(name = "greet", description = "Print a friendly greeting")]
pub struct Greet {
    #[arg(short, long, default_value = "world")]
    pub name: String,

    #[arg(long, default_value_t = false)]
    pub loud: bool,
}

#[async_trait]
impl TypedCommand for Greet {
    async fn run(self) -> Result<(), FrameworkError> {
        let prefix = if self.loud { "HELLO" } else { "Hello" };
        println!("{prefix}, {}!", self.name);
        Ok(())
    }
}
```

Drop that in `src/commands/greet.rs`, add `pub mod greet;` to `src/commands/mod.rs`, and run it:

```bash
cargo run --bin console -- greet
# Hello, world!
cargo run --bin console -- greet --name Alice --loud
# HELLO, Alice!
cargo run --bin console -- greet --help
# (clap-generated per-command help, including the typed flags)
```

No central registry to edit. `#[derive(Command)]` submits a `CommandEntry { name, description, clap_builder, handler }` via inventory; the console binary calls `suprnova::console::dispatch_argv(env::args)`, which builds one clap parser tree from every registered entry and routes the parsed `ArgMatches` to the right handler.

### The simpler path: raw `Vec<String>`

For trivial commands that don't need typed args, the `#[command]` attribute on an async fn works too:

```rust
use suprnova::{command, FrameworkError};

#[command(name = "ping", description = "Smoke test")]
pub async fn ping(_args: Vec<String>) -> Result<(), FrameworkError> {
    println!("pong");
    Ok(())
}
```

Under the hood both paths land in the same `CommandEntry` registry; the raw shape just uses a clap subcommand with a `trailing_var_arg` to capture argv into the `Vec<String>`. Prefer the typed shape for any command with arguments — you get per-command `--help`, value parsing, default values, and short/long flag pairs without writing a parser by hand.

## The Console Binary

`suprnova new` scaffolds two binaries into every new project:

- **`<project>`** (`cmd/main.rs` or `src/main.rs`) — the HTTP server, started by `cargo run` or `suprnova serve`. Long-running; serves until killed.
- **`console`** (`src/bin/console.rs`) — the runtime command dispatcher. One-shot; exits when the handler returns.

The console binary's `main` is small and predictable:

```rust
use std::process::ExitCode;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let _ = dotenvy::dotenv();

    my_app::config::register_all();
    my_app::bootstrap::register().await;

    let argv: Vec<String> = std::env::args().collect();
    match suprnova::console::dispatch_argv(argv).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
```

Tokio runs in `current_thread` flavor — there's no work to parallelize across cores in a one-shot command, and the multi-threaded runtime's worker pool would just be overhead.

`config::register_all()` + `bootstrap::register()` run in the same order as the server's `Application::run`. That means seeders, queue jobs, mail factories, and event listeners are all wired before any handler reaches for them. If you want a command to skip an expensive bootstrap step, gate the step itself on an env var rather than threading a "lazy bootstrap" flag through the framework.

## Built-in Commands

The framework registers a small set of commands itself. Linking the framework into a project pulls them in automatically.

| Command   | What it does                              |
|-----------|-------------------------------------------|
| `db:seed` | Run every registered `Seeder` in order.   |
| `help`    | List available commands. Also `--help`, `-h`, or empty argv. |

`db:seed` runs whatever you've registered in `bootstrap::register()` with `suprnova::seed::register::<MySeeder>()`. On an empty registry it logs a `tracing::warn!` and returns `Ok(())` — invoking `db:seed` before registering seeders is a benign user mistake, not a programmer error.

## Defining Commands

Two macros, one registry. Pick whichever fits the command's shape.

### `#[derive(Command)]` — typed args (recommended)

Goes on top of `#[derive(clap::Parser)]`. The struct fields are the command's args; clap parses argv into the struct; the framework calls your `TypedCommand::run(self)`.

```rust
use async_trait::async_trait;
use clap::Parser;
use suprnova::{Command, FrameworkError, TypedCommand};

#[derive(Parser, Command, Debug)]
#[console(name = "users:purge", description = "Purge users older than N days")]
pub struct UsersPurge {
    #[arg(long)]
    pub older_than_days: u32,

    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}

#[async_trait]
impl TypedCommand for UsersPurge {
    async fn run(self) -> Result<(), FrameworkError> {
        // self.older_than_days, self.dry_run — typed, validated by clap
        Ok(())
    }
}
```

Attributes:

| Attribute    | Required | Purpose                                       |
|--------------|----------|-----------------------------------------------|
| `#[console(name = "...")]` | yes | The invocation name on the CLI (`"users:purge"`, `"mail:send"`, `"greet"`). |
| `#[console(description = "...")]` | no | One-line description shown in top-level help. |
| `#[arg(...)]` (clap) | n/a | Clap's own field attributes for short/long flags, defaults, value parsers, etc. |

You also get clap's auto-generated per-command help (`console users:purge --help`) for free.

### `#[command]` — raw `Vec<String>` (simple cases)

For commands that take no arguments or only consume positionals as a list, the attribute on an async fn is enough:

```rust
use suprnova::{command, FrameworkError};

#[command(name = "cache:clear", description = "Drop every entry from the cache")]
pub async fn cache_clear(_args: Vec<String>) -> Result<(), FrameworkError> {
    suprnova::Cache::flush().await
}
```

The annotated function must be `async fn(Vec<String>) -> Result<(), FrameworkError>`. The macro preserves the original function, so you can also call it directly from Rust — useful for unit tests that don't want to thread argv strings through the dispatcher.

Names in both shapes support Laravel-style namespacing: `mail:send`, `queue:work`, `db:fresh`. The colon is purely cosmetic — it's a string the dispatcher matches against `argv[1]`.

## `suprnova make:command`

The CLI generator drops a runnable stub:

```bash
suprnova make:command cache:clear
# → src/commands/cache_clear.rs (with #[command(name = "cache:clear")])
# → src/commands/mod.rs gets `pub mod cache_clear;` appended
```

Name normalization:

| Input          | File              | Command name   |
|----------------|-------------------|----------------|
| `greet`        | `greet.rs`        | `greet`        |
| `CleanCache`   | `clean_cache.rs`  | `clean-cache`  |
| `clean-cache`  | `clean_cache.rs`  | `clean-cache`  |
| `mail:send`    | `mail_send.rs`    | `mail:send`    |

If the input contains `:`, the colon namespace is preserved verbatim. Otherwise the Rust fn name is snake_case and the command name is kebab-case.

Make sure `pub mod commands;` is declared in `src/lib.rs` so the inventory submission is link-reachable from the console binary. The generator scaffolds this for new projects; if you removed it, the new file's `#[command]` block will compile but never end up in the registry.

## Why a Per-Project Binary

The framework deliberately does **not** make a global `suprnova` CLI command for runtime tasks like `db:seed`. A global binary can't statically load your app's seeders, factories, or `#[command]` async fns without either:

- shelling out to `cargo run --bin app -- ...` (slow — full compile per invocation, defeats the point), or
- dynamic loading (too much complexity for v1)

So the user's project produces a `console` binary. Run it directly:

```bash
./target/debug/console db:seed
./target/release/console greet Alice
cargo run --bin console -- mail:send
```

This is the Rust analogue of Laravel's `php artisan` — same script, same process, same address space, exits when the handler returns.

### Asymmetry with `suprnova migrate`

There are two distinct command-invocation paths in a Suprnova project, and the asymmetry is **structural** — don't try to unify them:

| Command surface                           | Invocation                              | Why                                      |
|-------------------------------------------|-----------------------------------------|------------------------------------------|
| `suprnova new`, `suprnova make:*`, `suprnova serve` | Global CLI binary (installed via `cargo install --git`) | File-only generators; don't need user code. |
| `suprnova migrate`, `suprnova migrate:status`, etc. | Global CLI shells into `cargo run --bin app -- migrate` | Schema-level; the app binary's Clap parser handles it. |
| `console db:seed`, `console <your-command>` | Per-project `console` binary           | Needs user types (seeders, commands); compiled into the user's crate. |

The migrate path predates the Console architecture and continues to work as-is. New runtime commands belong in `#[command]` async fns dispatched by the project's `console` binary.

## Best Practices

### Keep handlers small; reach for shared services through the container

A `#[command]` is the CLI-shaped wrapper; the business logic should live in an `Action`, a service, or a method on a model. The handler parses args, resolves the service from the container, and forwards. That keeps the same logic testable from a unit test, an HTTP route, and the console.

```rust
#[command(name = "users:purge")]
pub async fn users_purge(args: Vec<String>) -> Result<(), FrameworkError> {
    let action: PurgeStaleUsers = App::get()?;
    action.execute(parse(args)?).await
}
```

### Use namespaces for related commands

Group with `:`: `mail:send`, `mail:retry`, `mail:queue:work`. The dispatcher treats it as opaque, but humans scan `mail:*` better than `send-mail`, `retry-mail`, `mail-queue-work`.

### Don't print structured data — return it

Console handlers print to stdout for human-readable output. If a downstream tool needs to consume the output, write a `console <name> --json` variant that emits machine-readable JSON to stdout and a status line to stderr. Don't make the human-readable path responsible for both audiences.

### Treat exit codes as the contract

`FrameworkError` → `ExitCode::FAILURE` is the only failure path. Don't `std::process::exit(custom_code)` from inside a handler — return `Err(...)` and let the binary's `main` translate. Future tooling (CI gates, supervised workers) only has to read the exit code.

## Reference

| Symbol                                    | Purpose                                       |
|-------------------------------------------|-----------------------------------------------|
| `suprnova::Command` (derive)              | Register a `clap::Parser`-deriving struct as a typed console command. Pairs with `TypedCommand`. |
| `suprnova::TypedCommand` (trait)          | Trait with `async fn run(self) -> Result<...>` — the body of a typed command. |
| `suprnova::command` (attribute)           | Register an async fn taking `Vec<String>` as a raw-args console command. |
| `suprnova::console::dispatch_argv(argv)`  | Build the clap parser tree from every registered entry, parse argv, route to the handler. |
| `suprnova::console::find(name)`           | Look up a registered command by exact name.   |
| `suprnova::console::list()`               | All registered commands, sorted by name.      |
| `suprnova::CommandEntry`                  | Inventory record: `{ name, description, clap_builder, handler }`. Submitted by both macros. |
| `suprnova::CommandHandler`                | The handler fn-pointer type: `fn(&clap::ArgMatches) -> Pin<Box<dyn Future<...>>>`. |
