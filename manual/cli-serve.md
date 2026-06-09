# suprnova serve

`suprnova serve` runs your backend and the Vite dev server together with hot
reload on both sides, plus automatic TypeScript type regeneration whenever you
touch a `#[derive(InertiaProps)]` struct. It's the one command you keep open
in a terminal while you're building.

```bash
suprnova serve
```

Both processes stream their stdout into the same terminal with coloured
`[backend]` and `[frontend]` prefixes so you can tell who said what. `Ctrl+C`
shuts them both down cleanly.

## Usage

```bash
suprnova serve [OPTIONS]
```

| Option | Default | Description |
|---|---|---|
| `-p, --port <PORT>` | `8000` (CLI) / `$SERVER_PORT` (env) | Backend HTTP port |
| `--frontend-port <PORT>` | `5173` (CLI) / `$VITE_PORT` (env) | Vite dev server port |
| `--backend-only` | `false` | Skip the Vite dev server |
| `--frontend-only` | `false` | Skip the backend, just run Vite |
| `--skip-types` | `false` | Don't regenerate TypeScript types on Rust changes |

The CLI flags take precedence over environment variables, which take precedence
over the built-in defaults. A scaffolded `.env` ships with `SERVER_PORT=8080`
and `VITE_PORT=5173`; you'll see those values used unless you override with
`--port`.

## Examples

### Default — both servers

```bash
suprnova serve
```

Output:

```
Backend  http://127.0.0.1:8765
Frontend http://127.0.0.1:5765
[backend] Compiling my-app v0.1.0 ...
[frontend] VITE v6.3.0  ready in 312 ms
```

Hit `http://127.0.0.1:8765` in your browser. The backend serves the Inertia
HTML shell and proxies asset requests through to Vite, so you don't need to
visit the Vite URL directly.

### Custom ports

```bash
suprnova serve --port 3000 --frontend-port 3001
```

Or set them in `.env` and run without flags:

```env
SERVER_PORT=3000
VITE_PORT=3001
```

### Backend only

```bash
suprnova serve --backend-only
```

Good for working on an API-only project, or when your frontend is already
running in another terminal (or another machine, or a deployed preview).

### Frontend only

```bash
suprnova serve --frontend-only
```

Good for working on UI without paying the cost of a Rust rebuild on every
save, or when the backend is running in another shell (or in Docker).

### Skip type generation

```bash
suprnova serve --skip-types
```

Disables the TypeScript regeneration watcher. Use this when you're managing
`frontend/src/types/inertia-props.ts` by hand, or when you're working far
from any Inertia code and want quieter output.

## What it actually does

When you run `suprnova serve`, the CLI:

1. Loads `.env` from the current directory.
2. Resolves backend and frontend ports (CLI flag → env var → default).
3. Verifies you're in a Suprnova project — `Cargo.toml` must exist (unless
   `--frontend-only`) and a `frontend/` directory must exist (unless
   `--backend-only`).
4. Regenerates TypeScript types from any `#[derive(InertiaProps)]` structs
   it finds in `src/`, writing them to `frontend/src/types/inertia-props.ts`.
5. Installs `cargo-watch` via `cargo install cargo-watch` if it isn't on the
   PATH yet (one-time, with a "Installing..." notice). Skipped under
   `--frontend-only`.
6. Runs `npm install` in `frontend/` if `node_modules` doesn't exist yet.
   Skipped under `--backend-only`.
7. Spawns `cargo watch -x 'run --bin <package-name>'` for the backend.
   `cargo-watch` re-runs the binary whenever a `.rs` file changes.
8. Spawns `npm run dev` in `frontend/` for Vite, which gives you HMR for
   Svelte/React/Vue components and Tailwind classes.
9. Starts a file watcher on `src/` that re-runs the type generator (with
   500 ms debounce) whenever a `.rs` file changes.
10. Forwards both children's stdout/stderr to your terminal with `[backend]`
    and `[frontend]` prefixes.

`Ctrl+C` signals the manager to set its shutdown flag, kill both children,
and exit. If either process exits on its own — usually because of a Rust
compile error too severe for `cargo watch` to recover, or a port conflict —
the manager treats that as a shutdown signal and tears down the other.

### Why Suprnova diverges

Laravel users typically run `php artisan serve` for the backend and `npm
run dev` in another terminal, and most teams paper over the two-terminal
split with a `Procfile` and `foreman`/`overmind`. Suprnova ships that
multiplexer as a first-class CLI command. You get one terminal, one
`Ctrl+C`, automatic toolchain bootstrap (`cargo-watch`, `npm install`),
and a typed-Inertia bridge that regenerates `frontend/src/types/inertia-props.ts`
on the fly so your Svelte/React/Vue components always see the current
prop shape without manual type sync.

## Hot reload

**Backend.** `cargo watch -x 'run --bin <package>'` is the loop. It rebuilds
and restarts the server on every `.rs` change in the project. Cold rebuilds
after touching a heavy crate can take several seconds; incremental changes
in a single file are usually sub-second.

**Frontend.** Vite's HMR injects component changes in place without a full
reload, preserving component state. Tailwind classes update live via the
Tailwind v4 watcher.

**TypeScript types.** Whenever a `.rs` file changes, the type watcher re-runs
the generator. If new `#[derive(InertiaProps)]` structs appear (or existing
ones change shape), the regenerated `frontend/src/types/inertia-props.ts`
triggers Vite's HMR for the component that imports them.

## Troubleshooting

### Port already in use

```text
[backend] Error: Address already in use (os error 98)
```

Find and kill the process, or pick another port:

```bash
lsof -i :8765
kill -9 <pid>

# or
suprnova serve --port 8081
```

### `cargo-watch` install fails

The CLI runs `cargo install cargo-watch` if it isn't already on PATH. If
that install fails (no network, restricted environment), install it manually
once:

```bash
cargo install cargo-watch
```

After that, `suprnova serve` will find it and won't try to install again.

### Frontend dependencies stuck

If `npm install` fails mid-bootstrap, fix the cause (npm registry reachable,
disk space, lockfile in good shape) and run it manually:

```bash
cd frontend && npm install
```

Then re-run `suprnova serve`. The CLI only auto-runs `npm install` when
`node_modules` is missing, so a successful manual install lets it skip that
step.

### Type regeneration not picking up changes

The watcher polls every 2 seconds (using `notify` with a poll interval —
chosen for cross-platform reliability over inotify quirks) and debounces
regeneration to once every 500 ms. If a change isn't showing up:

- Confirm the file is under `src/` (the watcher doesn't recurse into
  `crates/`, `cmd/`, or `migrations/`).
- Confirm the struct actually has `#[derive(InertiaProps)]`.
- Restart `suprnova serve` and watch for the `Generated N type(s)` startup
  message — if you see `No InertiaProps structs found`, the scanner didn't
  find anything to emit.

### Backend exits silently right after start

When either child process exits, the manager shuts the other down too. If
the backend died with a compile error, the `[backend]` lines just above
the "Servers stopped." message will show the `error[E…]` from rustc. Fix
the compile error and re-run.

## Next

- [Installation](installation.md) — get the CLI on your machine
- [Quickstart](quickstart.md) — a full first-app walkthrough
- [Directory Structure](structure.md) — what `suprnova new` scaffolded
- [Generators](cli-generators.md) — `make:controller`, `make:action`, etc.
- [Console](console.md) — the per-project `cargo run --bin console` binary
