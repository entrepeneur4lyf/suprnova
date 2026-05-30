# CLI Overview

Suprnova ships two binaries with different jobs. The global `suprnova` —
installed once into `~/.cargo/bin` — scaffolds new projects, generates code,
boots dev servers, and runs migrations. The per-project `console`, built
from each app's `src/bin/console.rs`, runs runtime commands that need the
app's compiled types (seeders, pruners, your own `#[command]` handlers).
This chapter is the map; each subcommand has its own deep-dive in the
sibling chapters listed under [Next](#next).

## Install

The CLI is distributed via `cargo install --git`. Suprnova isn't on
crates.io yet — see the [Pre-launch note in
Installation](installation.md#pre-launch-note) for why.

```bash
cargo install --git https://github.com/entrepeneur4lyf/suprnova.git suprnova-cli
suprnova --version
```

To upgrade later, pass `--force`:

```bash
cargo install --force --git https://github.com/entrepeneur4lyf/suprnova.git suprnova-cli
```

## The two binaries

| Binary | Built from | Used for |
|---|---|---|
| `suprnova` | `suprnova-cli/` (this crate) | Scaffolding (`new`), generators (`make:*`), dev runner (`serve`), migrations (`migrate*`, `db:sync`), Docker config (`docker:*`), SSR worker (`ssr:*`), key minting (`key:generate`), type generation (`generate-types`) |
| `console` | `src/bin/console.rs` in your project | Runtime commands that link your app's types — built-in `db:seed` and `model:prune` plus every `#[command]` / `#[derive(Command)]` you define |

Worker daemons (`schedule:run`, `schedule:work`, `schedule:list`,
`workflow:work`, `queue:work`) sit on a third surface: your *app* binary's
own clap parser, the same binary that serves HTTP. The global `suprnova`
shells into `cargo run --quiet -- <name>` for those so you can launch them
from the CLI you already have open. See [Console](console.md) for the full
three-way split.

### Why Suprnova diverges

Laravel solves this with a single per-project script — `php artisan` —
because PHP loads framework and user code together at runtime. Rust links
binaries at compile time, so a global `suprnova` binary can't statically
see your seeders, factories, or `#[command]` handlers. The pragmatic split:

- File-only work (scaffolding, generators, ops) lives on the global
  `suprnova` binary
- Runtime work that needs your compiled types lives on the per-project
  `console` binary
- Daemons live on your app/server binary so they share the same boot path
  as `serve`

You get the ergonomics of `php artisan` (`cargo run --bin console -- db:seed`
or `console <name>` directly) without the static-linking lie.

## Commands at a glance

The same list `suprnova --help` prints, grouped the same way.

### Create

| Command | Description |
|---|---|
| `suprnova new [name]` | Scaffold a new project. See [`suprnova new`](cli-new.md). |
| `suprnova serve` | Boot backend + Vite together with hot reload. See [`suprnova serve`](cli-serve.md). |
| `suprnova web:run` | Run the app binary directly (no Vite, no rebuild loop). Production-shaped local run. |

### Generate

| Command | Description |
|---|---|
| `suprnova make:controller <name>` | Scaffold a controller in `src/controllers/`. |
| `suprnova make:action <name>` | Scaffold an invokable action in `src/actions/`. |
| `suprnova make:middleware <name>` | Scaffold a middleware in `src/middleware/`. |
| `suprnova make:migration <name>` | Scaffold a SeaORM migration in `src/migrations/`. |
| `suprnova make:inertia <name>` | Scaffold an Inertia page in `frontend/src/pages/`. Pass `--data` for a `#[derive(Data, Validate)]` props struct in `src/props/` instead. |
| `suprnova make:error <name>` | Scaffold a domain error in `src/errors/`. |
| `suprnova make:task <name>` | Scaffold a scheduled task in `src/tasks/`. |
| `suprnova make:command <name>` | Scaffold a `#[derive(Command)]` console command in `src/commands/`. |
| `suprnova generate-types` | Emit TypeScript types from every `#[derive(InertiaProps)]` struct. `-o <path>` to override output, `-w` to watch and regenerate. |

See [Generators](cli-generators.md) for the full scaffold details and what
each generated file looks like.

### Database

| Command | Description |
|---|---|
| `suprnova migrate` | Run all pending migrations. |
| `suprnova migrate:status` | Show which migrations are applied vs pending. |
| `suprnova migrate:rollback [--step N]` | Roll back the last N migrations (default 1). |
| `suprnova migrate:fresh` | Drop every table and re-run all migrations. **Destructive.** |
| `suprnova db:sync [--skip-migrations] [--regenerate-models]` | Run migrations and regenerate SeaORM entities from the live schema. `--regenerate-models` overwrites custom model files in `src/models/`. |

`db:seed` is **not** here — it lives on the per-project `console` binary
because the seeder registry is compiled into your crate. Run it via
`cargo run --bin console -- db:seed` or `./target/debug/console db:seed`.
See [Console](console.md) for the registration pattern.

See [Migrations chapter](cli-migrations.md) for the full migration workflow.

### Schedule

| Command | Description |
|---|---|
| `suprnova schedule:run` | Run every due task once. The cron-friendly form. |
| `suprnova schedule:work` | Foreground daemon that checks every minute and runs due tasks. |
| `suprnova schedule:list` | Print every registered task with its cron expression. |

Each of these shells into `cargo run --quiet -- <name>` against your
app/server binary — the same binary that serves HTTP — so registered tasks
and bootstrapped services are visible. See [Scheduling
CLI](cli-scheduling.md) and the [Scheduling](scheduling.md) chapter.

### Workflow

| Command | Description |
|---|---|
| `suprnova workflow:work` | Start the workflow worker daemon. Pulls workflow steps off the registry and runs them with the same panic boundary as HTTP handlers. |
| `suprnova workflow:install` | Drop the workflow + workflow_steps migrations into `src/migrations/`. Already present in fresh scaffolds. |

See [Workflows](workflows.md).

### SSR

| Command | Description |
|---|---|
| `suprnova ssr:start [--runtime node\|bun\|deno] [--bundle <path>]` | Launch the Inertia SSR worker in the foreground. Falls back to `SUPRNOVA_SSR_RUNTIME` env, then `node`; bundle falls back to `SUPRNOVA_SSR_BUNDLE`, then `frontend/bootstrap/ssr/ssr.js`. |
| `suprnova ssr:check [--url <url>] [--timeout-ms N]` | Probe the SSR worker. Falls back to `SUPRNOVA_SSR_URL`, then `http://127.0.0.1:13714`. Timeout default 2000 ms. |

See [Inertia SSR](frontend.md) for the production setup.

### Deploy

| Command | Description |
|---|---|
| `suprnova docker:init` | Emit a multi-stage production `Dockerfile` + `.dockerignore`. |
| `suprnova docker:compose [--with-mailpit] [--with-minio]` | Emit a `docker-compose.yml` for local development. Postgres + Redis always included; Mailpit and MinIO opt in. |

See [Docker](cli-docker.md) and the [Deployment](deployment.md) chapter.

### Security

| Command | Description |
|---|---|
| `suprnova key:generate [--show]` | Mint a 32-byte AES-256 key, base64 URL-safe no padding (same wire format `EncryptionKey::to_base64` produces). `--show` prints just the key for `APP_KEY=$(suprnova key:generate --show)`. |

See [Encryption](encryption.md) for what `APP_KEY` protects and how
rotation via `APP_KEY_PREVIOUS` works.

## Quick start

The most common path from "nothing installed" to "running app":

```bash
# 1. Install the CLI
cargo install --git https://github.com/entrepeneur4lyf/suprnova.git suprnova-cli

# 2. Scaffold a project (interactive — picks Svelte by default)
suprnova new my-app

# 3. Boot it
cd my-app
suprnova migrate
npm install
suprnova serve
```

Non-interactive scaffold (CI, scripted setup):

```bash
suprnova new my-app \
  --frontend svelte \
  --no-interaction \
  --no-git
```

API-only scaffold (no Inertia, no SPA):

```bash
suprnova new my-api --api
```

Generate code in an existing project:

```bash
suprnova make:controller Posts
suprnova make:migration create_posts_table
suprnova make:command reports:daily   # registers under the per-project console binary
suprnova migrate
```

## Getting help

`--help` (or `-h`) works on any subcommand. The top-level help is
hand-formatted (`ui::print_help`) and groups commands by section; the
per-subcommand help comes from clap and shows every flag with its default:

```bash
suprnova --help
suprnova new --help
suprnova serve --help
suprnova make:inertia --help
```

For the per-project `console` binary:

```bash
cargo run --bin console -- --help
cargo run --bin console -- db:seed --help
cargo run --bin console -- <your-command> --help
```

## Next

- [`suprnova new`](cli-new.md) — every flag the scaffolder accepts and the
  directory layout it produces
- [`suprnova serve`](cli-serve.md) — the dev runner: backend + Vite + type
  generation
- [Generators](cli-generators.md) — the full `make:*` family with output
  templates
- [Migrations CLI](cli-migrations.md) — `migrate`, `migrate:fresh`,
  `db:sync`, and the SeaORM workflow
- [Console](console.md) — the per-project `console` binary, `#[command]`,
  `#[derive(Command)]`, and the three-binary asymmetry
