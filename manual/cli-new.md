# suprnova new

`suprnova new` scaffolds a Suprnova project — a fresh Cargo crate with
controllers, routes, migrations, an Inertia SPA, and a working auth
flow already wired together. Run it once per app, then live in
`suprnova serve` from there.

## Usage

```bash
suprnova new [name] [options]
```

If `name` is omitted, the interactive wizard prompts for it. The
name becomes the project directory, the Cargo package name (after
snake-casing), and the default `APP_NAME` in `.env`. Names must be
ASCII letters/digits/`-`/`_`, start with a letter, contain no path
separators or `..`, and be 64 characters or fewer.

## Options

| Option | Description |
|---|---|
| `--frontend <svelte\|react\|vue>` | Pick the SPA framework non-interactively. Conflicts with `--api`. |
| `--api` | Scaffold a JSON:API-only project (no Inertia, no SPA, token auth instead of sessions). |
| `--no-interaction` | Skip all prompts and use defaults (name `my-suprnova-app`, frontend `svelte`, empty author/description). |
| `--no-git` | Skip `git init` in the new project. |

## Interactive mode

```bash
suprnova new my-app
```

The wizard asks four questions, in this order:

1. **Project name** — defaults to the directory argument (`my-app`)
2. **Description** — used as the Cargo package description
3. **Author** — used as the Cargo package author; defaults to your
   `git config user.name <name@email>` if set
4. **Frontend framework** — `Svelte (recommended)`, `React`, or `Vue`

After confirming, the scaffolder writes the project, runs `git init`
(unless `--no-git`), and prints the next steps:

```
Backend  http://localhost:8000
Frontend http://localhost:5173
```

## Non-interactive mode

For CI, dotfiles, or scripted setup, pass `--no-interaction` plus the
flags you want to override:

```bash
suprnova new my-app --frontend svelte --no-interaction
```

Defaults under `--no-interaction`:

- Frontend: `svelte`
- Description: `"A web application built with Suprnova"`
- Author: empty
- Git: initialized

There are no `--description` or `--author` flags; those values are
only set via the interactive prompts or accept their defaults.

## API-only project

For service backends with no SPA, use `--api`:

```bash
suprnova new my-api --api
```

The API starter is significantly smaller: no `frontend/` directory, no
Inertia, no auth views, single-crate `src/main.rs` layout (instead of
the SPA starter's `cmd/main.rs` workspace), token-based auth, and a
sample `users` controller plus `UserResource` JSON serializer. The API
starter binds to port 8080 in its `.env`.

`--api` is mutually exclusive with `--frontend`; passing both errors.
Under `--api`, only the project name is prompted — the
description/author/frontend prompts are skipped.

## What gets scaffolded

A full directory tour lives in [Directory Structure](structure.md);
the short version is:

- `cmd/main.rs` — binary entry; calls `Application::new()…run()`
- `src/` — controllers, actions, commands, config, middleware,
  models, migrations, plus `bootstrap.rs` and `routes.rs`
- `src/bin/console.rs` — the per-project `php artisan` analogue
- `frontend/` — Vite 6 + Tailwind v4 + your chosen framework, with
  Home / Dashboard / Login / Register pages already wired through
  Inertia
- `src/migrations/` — `users`, `sessions`, and `remember_tokens`
  tables ready to go
- `.env` — SQLite database by default, with a freshly-generated
  `APP_KEY` so the app boots without operator intervention
- `.gitignore`, `Cargo.toml`

### Why Suprnova diverges

Laravel ships with Blade and pulls a frontend in via Breeze/Jetstream
after the fact. Suprnova goes the other way: `suprnova new` always
scaffolds either a real SPA (Svelte/React/Vue on Inertia) or a real
JSON:API project. There is no template-engine-first starter — if you
want server-rendered HTML, Tera is available, but it's not the default
shape and there's no scaffolder path that puts views in the front of
your app.

The default frontend is **Svelte 5** (runes-on), not React. We picked
it because it's the lightest of the three at runtime and the closest
to the framework's "compile-time wins over runtime cleverness"
philosophy. React and Vue are equally first-class — pick what your
team knows.

## Distribution

The CLI itself ships via git, not crates.io (pre-launch):

```bash
cargo install --git https://github.com/entrepeneur4lyf/suprnova.git suprnova-cli
```

`--force` on the same command updates an existing install. Scaffolded
projects depend on the framework crate the same way — a git
dependency in their `Cargo.toml` — and `cargo update -p suprnova`
pulls the latest. See [Installation](installation.md) for the full
toolchain prerequisites.

## Next

- [Installation](installation.md) — Rust/Node/DB prerequisites and
  toolchain setup
- [Directory Structure](structure.md) — what each scaffolded file
  does
- [Quickstart](quickstart.md) — first 5 minutes after `suprnova new`
- [suprnova serve](cli-serve.md) — the dev runner you'll use next
- [Console](console.md) — `cargo run --bin console` and the
  `#[command]` system
