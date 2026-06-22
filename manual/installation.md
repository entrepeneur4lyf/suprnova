# Installation

This chapter gets you from "no Suprnova on this machine" to a running
scaffolded project. If you're already there, jump to the
[Quickstart](quickstart.md).

## Requirements

- **Rust 1.85+** (the workspace uses the 2024 edition). Install via
  [rustup](https://rustup.rs/):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **Node.js 20+** and **npm** (or pnpm/yarn/bun) for the frontend
  toolchain. Suprnova uses Vite 8 and your starter ships TypeScript +
  Tailwind v4. Install via [nodejs.org](https://nodejs.org/) or your
  package manager.
- **A database client library** that matches the driver you want to use:
  - SQLite — no extras needed; sqlite is bundled
  - PostgreSQL — `libpq` on most systems (often pre-installed)
  - MySQL or MariaDB — `libmariadb` / `libmysqlclient` on most systems

You don't have to choose a database now. The default scaffolder picks
SQLite so a fresh app runs with zero setup.

## Install the CLI

Suprnova is distributed as a Cargo project, and the CLI installer pulls
the framework from git (not from crates.io — see the [Pre-launch
note](#pre-launch-note) below):

```bash
cargo install --git https://github.com/entrepeneur4lyf/suprnova.git suprnova-cli
```

This compiles the `suprnova` binary and drops it into `~/.cargo/bin`.
Confirm it worked:

```bash
suprnova --version
```

You should see `suprnova 0.x.x`.

If `suprnova` isn't found, your `~/.cargo/bin` isn't on `PATH`. Add this
to your shell config:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

## Create a project

`suprnova new` scaffolds a complete project — backend + chosen frontend
+ Vite config + auth migrations + sample routes. It's interactive by
default:

```bash
suprnova new my-app
```

The wizard asks for, in order:

1. **Project name** — skipped when you pass it as the argument (`my-app`)
2. **Description** — used in `Cargo.toml`
3. **Author** — used in `Cargo.toml`; defaults to your git `user.name`
4. **Frontend framework** — one of `svelte` (default), `react`, `vue`

If you want to skip the prompts (CI, scripted setup), pass
`--no-interaction` and pick a frontend explicitly:

```bash
suprnova new my-app --frontend svelte --no-interaction
```

`--no-interaction` accepts the defaults for description ("A web
application built with Suprnova") and author (empty). To set those,
edit the generated `Cargo.toml` after scaffolding.

The three frontend choices each ship their own runes-on/Svelte-5,
React-19, or Vue-3.5 starter. All three use Inertia v3 + Vite 8 +
Tailwind v4 and pre-wire a Login/Register/Dashboard flow with
session-based auth.

Suprnova also ships a slimmer **API starter** for service backends with
no SPA:

```bash
suprnova new my-api --api
```

The API starter has the same backend stack but no frontend, no Inertia,
and uses token-based auth instead of session cookies.

## First run

```bash
cd my-app

# Run migrations (users, sessions, etc.)
suprnova migrate

# Install frontend dependencies
npm install              # in the project root

# Start the backend + Vite together
suprnova serve
```

`suprnova serve` runs the backend on `http://127.0.0.1:8765` and Vite
on `http://127.0.0.1:5765`. Hit the backend URL — Vite is proxied so
you don't need to visit it directly.

You should see the welcome page. Then visit `/register` to make an
account and `/login` to log in.

## What got scaffolded

```
my-app/
├── Cargo.toml          # crate manifest, two [[bin]] targets
├── .env                # local config (DB URL, app key, ports)
├── .env.example        # template for ops/CI
├── .gitignore
├── cmd/
│   └── main.rs         # the binary entry; calls Application::new().run()
├── src/
│   ├── lib.rs          # module wiring
│   ├── bootstrap.rs    # service registration (the Suprnova analogue of providers)
│   ├── routes.rs       # the routes! macro tree
│   ├── bin/
│   │   └── console.rs  # `cargo run --bin console <subcommand>`
│   ├── actions/        # single-method invokable controllers
│   ├── commands/       # `#[command]`-annotated handlers
│   ├── config/         # typed config sections (database, mail)
│   ├── controllers/    # home, auth, dashboard
│   ├── middleware/     # logging, authenticate
│   ├── migrations/     # SeaORM migrators (users, sessions, etc.)
│   └── models/         # `#[suprnova::model]` structs (user)
├── frontend/
│   ├── package.json
│   ├── vite.config.ts
│   ├── tsconfig.json
│   ├── index.html
│   └── src/
│       ├── main.{tsx,ts}
│       ├── app.css
│       ├── pages/
│       │   ├── Home, Dashboard
│       │   └── auth/{Login,Register}
│       └── types/
│           └── inertia-props.ts
└── public/
    └── assets/         # Vite production build output
```

The full directory tour is in [Directory Structure](structure.md).

## Updating the CLI

The CLI lives in your `~/.cargo/bin`. To update to the latest:

```bash
cargo install --force --git https://github.com/entrepeneur4lyf/suprnova.git suprnova-cli
```

`--force` makes Cargo overwrite the existing binary.

## Updating your app's framework version

A scaffolded app depends on the `suprnova` framework crate via a git
dependency in `Cargo.toml`:

```toml
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }
```

To pull the latest framework changes:

```bash
cargo update -p suprnova
```

The git dependency tracks the default branch, so `cargo update -p suprnova`
always moves you to the latest framework. Your `Cargo.lock` records the
exact commit it resolved, so builds stay reproducible between updates —
there's no need to hand-pin a `rev` in `Cargo.toml`.

## Distribution model

Suprnova is distributed through git, not crates.io — both the framework
and the CLI install from GitHub. Each version is published as a tagged
GitHub Release (e.g. `v0.4.0`) for the changelog, but you don't depend on
the tag: the git dependency tracks the default branch, and `Cargo.lock`
pins the exact commit your app resolved, so builds are reproducible between
`cargo update` runs — no need to hand-pin a `tag` or `rev`.

## Editor setup

A few VS Code extensions make the experience smoother:

- **rust-analyzer** — the Rust language server
- **Svelte for VS Code** (or React/Vue if you chose those)
- **Tailwind CSS IntelliSense**
- **Even Better TOML**

`rust-analyzer` will index the project on first open; expect 1–2
minutes the first time, then incremental.

## Next

- [Quickstart](quickstart.md) — build a tiny app in 5 minutes
- [Directory Structure](structure.md) — what's in each file the
  scaffolder generated
- [Configuration](configuration.md) — the `.env` and typed config story
- [Routing](routing.md) — add your first route
