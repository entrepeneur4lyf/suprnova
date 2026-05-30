# Development

The day-to-day Suprnova loop is one command: `suprnova serve`. It runs
the Rust backend, the Vite frontend, and a TypeScript-types regenerator
in a single process, each watching the right files. This chapter covers
the dev server, how the hot-reload pieces fit together, and the
commands you'll reach for daily. For first-time setup see
[Installation](installation.md); for the directory tour see
[Directory Structure](structure.md).

## The dev server

From a scaffolded project's root:

```bash
suprnova serve
```

The CLI prints two URLs and then a continuous stream of prefixed
output from each child process:

```
Backend  http://127.0.0.1:8000
Frontend http://127.0.0.1:5173

[backend]  Compiling links v0.1.0
[backend]  Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.21s
[backend]  Running `target/debug/links`
[frontend] VITE v6.0.1  ready in 312 ms
[frontend]   ➜  Local:   http://localhost:5173/
[types]    Watching for Rust file changes to regenerate types
```

You hit the backend URL (`127.0.0.1:8000`). Vite serves your JS/CSS
through Inertia's dev integration — you don't visit `:5173` directly.
Press `Ctrl+C` once and the CLI shuts both children down cleanly.

### Flags

| Flag | Default | What it does |
|---|---|---|
| `-p`, `--port <N>` | `8000` | Backend port |
| `--frontend-port <N>` | `5173` | Vite port |
| `--backend-only` | off | Skip the Vite child (API-only work) |
| `--frontend-only` | off | Skip the backend child (component work against a running backend elsewhere) |
| `--skip-types` | off | Skip the TypeScript-type generator + its watcher |

The same ports can be set in `.env` via `SERVER_PORT` and `VITE_PORT`.
A flag on the command line wins over `.env`.

### What it pre-flights

Before spawning anything, `suprnova serve`:

1. **Checks you're in a project.** Aborts with a clear error if there's
   no `Cargo.toml` (or no `frontend/` when running the frontend).
2. **Generates TypeScript types once.** Scans `src/` for
   `#[derive(InertiaProps)]` and writes
   `frontend/src/types/inertia-props.ts`. Skipped by `--skip-types` or
   `--frontend-only`.
3. **Installs `cargo-watch` if missing.** First run on a new machine
   runs `cargo install cargo-watch` for you, then continues.
4. **Runs `npm install` if `frontend/node_modules` is missing.** No
   manual install step on a fresh clone.

## Hot reload

Three watchers run concurrently inside `suprnova serve`:

- **`cargo watch -x 'run --bin <pkg>'`** drives the backend. Any `.rs`
  change under the project triggers a recompile and an in-process
  restart. Compile errors print to the `[backend]` stream and the
  previous binary stays up until the next successful build.
- **Vite** drives the frontend. Component, style, and asset edits
  hot-module-replace into the open browser tab without a full reload.
- **`notify`-based type watcher** reruns the InertiaProps scanner
  whenever a `.rs` file changes. It debounces at 500ms so a burst of
  saves regenerates `inertia-props.ts` once. Output appears under the
  `[types]` prefix.

That third one is the bit you don't have to think about: rename a field
on a `#[derive(InertiaProps)]` struct and the matching TypeScript
interface follows on the next save. The Svelte/React/Vue page picks
the new type up immediately. No `suprnova generate-types` invocation
needed during normal dev.

### Why Suprnova diverges

Most Rust web stacks make hot reload your problem — pick your own
file watcher, write your own restart wrapper, run Vite in a separate
terminal. Most Laravel stacks make TypeScript types your problem —
declare them in two places (PHP and TS) and keep them in sync.
`suprnova serve` runs both watchers, plus the type generator that
keeps your frontend types honest, as one supervised process. The
Tokio runtime makes "many things at once" cheap enough that a dev
loop can spend it freely.

## Day-to-day commands

The handful you'll run hourly:

```bash
suprnova serve                    # start dev (backend + Vite + type watcher)
suprnova make:controller orders   # scaffold a controller
suprnova make:migration add_idx   # scaffold a migration
suprnova db:sync                  # run migrations, regenerate SeaORM entities
suprnova migrate:status           # see what's applied
suprnova migrate:fresh            # drop tables + re-run from scratch
suprnova key:generate --show      # rotate APP_KEY
cargo run --bin console <cmd>     # any `#[command]`-annotated console handler
cargo test                        # run the test suite
```

`db:sync` is the dev shortcut for "migration + entity regen in one
step." In production you use plain `suprnova migrate` because you
don't want regeneration to happen on a release box. The full generator
surface is in [Code Generators](cli-generators.md) and the migration
verbs are in [Migrations](migrations.md).

## Debugging

### Logging

Suprnova uses `tracing` end-to-end. Filter what gets printed with
`LOG_LEVEL` (the same syntax as `tracing-subscriber`'s `EnvFilter`):

```bash
# Verbose framework output
LOG_LEVEL=debug suprnova serve

# Quiet hyper but verbose your crate
LOG_LEVEL=info,my_app=debug,hyper=warn suprnova serve
```

Output format is controlled by `LOG_FORMAT` (`pretty` for human-readable,
`json` for machine-parseable). The dev default is `pretty`. See
[Observability](observability.md) for the full logging surface.

### SQL queries

Turn on per-query logging with one env var:

```env
DB_LOGGING=true
```

This routes every SeaORM query through `tracing` at `info` so you can
see exactly what's executing. Leave it off in production unless you're
chasing a specific slow query — the volume gets noisy fast.

### Backtraces

Standard Rust:

```bash
RUST_BACKTRACE=1 suprnova serve
```

A panic in a handler is caught and turned into a structured 500
response; the backtrace lands in your logs without taking the server
down. See [Error Model](error-model.md) for how that contract works.

## Tests in the loop

```bash
cargo test                        # whole workspace
cargo test -p my_app              # just your app crate
cargo test some_test_name         # filter by name
cargo test -- --nocapture         # show println!/tracing output
```

Test execution is plain Cargo. The framework-side helpers
(`#[suprnova_test]`, `TestDatabase`, `expect!`, fakes for Mail/Queue/
Storage/etc.) are documented in [Testing](testing.md) and
[Database Testing](database-testing.md). They run under the same
`cargo test` you already know.

## Working with the SSR worker

If your app uses Inertia server-side rendering, you'll want the SSR
worker alongside `suprnova serve` during dev:

```bash
# Terminal 1
suprnova serve

# Terminal 2
suprnova ssr:start
```

`ssr:start` runs the bundled SSR worker under Node, Bun, or Deno
(`--runtime`). `ssr:check` verifies a running worker is reachable.
Both are documented under the frontend chapter — see
[Frontend](frontend.md).

## When something looks wrong

A short triage list for the most common dev-loop hiccups:

- **Port already in use.** Another `suprnova serve` is still up, or a
  prior backend wedged. `lsof -i :8000` to find it, or just pass
  `--port 8001`.
- **`cargo-watch` keeps recompiling.** Some editor is rewriting files
  on save (formatters, linters with autofix). Disable on-save format
  for the project, or scope your watcher with `CARGO_WATCH_IGNORE`
  patterns.
- **TypeScript types not updating.** Either `--skip-types` was passed,
  or the watcher tripped over a `.rs` parse error. Look at the
  `[types]` lines — it prints a warning and continues rather than
  failing the whole serve.
- **Vite errors but the backend is fine.** Run `npm install` in
  `frontend/` once (the CLI does this on first serve, but if you
  blow away `node_modules` it won't redo it until that directory is
  missing again on a fresh start).

Anything else, the [Errors](errors.md) chapter covers deeper triage
patterns.

## Next

- [Installation](installation.md) — first-time setup of the CLI and a
  project
- [Quickstart](quickstart.md) — build a tiny app end-to-end
- [Directory Structure](structure.md) — what each directory holds
- [Code Generators](cli-generators.md) — every `make:*` command
- [Testing](testing.md) — `#[suprnova_test]`, fakes, and the test
  database
