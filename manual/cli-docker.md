# Docker

Suprnova ships two CLI commands that generate Docker artifacts you can
adopt verbatim or modify. `docker:init` writes a multi-stage `Dockerfile`
+ `.dockerignore` for production. `docker:compose` writes a
`docker-compose.yml` for local development services (database, cache, and
optionally Mailpit + MinIO). Both commands write into the current project
root; neither tries to drive your container runtime.

## docker:init

Generate a production Dockerfile alongside a matching `.dockerignore`.

```bash
suprnova docker:init
```

The command refuses to overwrite an existing `Dockerfile`; remove the
existing file first if you want to regenerate.

### What gets written

| File | Purpose |
|------|---------|
| `Dockerfile` | Three-stage build: frontend assets, Rust release binary, runtime image |
| `.dockerignore` | Excludes `target/`, `node_modules/`, `.env*`, the existing build artifacts, and the Docker files themselves |

### Dockerfile shape

The generated Dockerfile uses three stages so the runtime image carries
only the compiled binary plus its required shared libraries:

1. **`frontend-builder`** — `node:20-alpine`. Installs npm deps and runs
   `npm run build`, producing `frontend/dist`.
2. **`backend-builder`** — `rust:1.75-slim-bookworm`. Caches `Cargo.toml`
   + `Cargo.lock` as a dependency layer, then copies your `cmd/`, `src/`,
   and the built `frontend/dist` (as `public/assets`) and runs
   `cargo build --release`.
3. **`runtime`** — `debian:bookworm-slim` with `ca-certificates` and
   `libssl3`. Runs as a non-root `appuser`. Copies the binary in as
   `./app` and the `public/` directory beside it. Exposes port 8765.

The final image's default `CMD` is `["./app"]`, which runs the unified
binary's `serve` subcommand (web server with auto-migrations on
startup). To run a different subcommand, override the command at
`docker run` time:

```bash
# Web server (default)
docker run -p 8765:8765 --env-file .env.production my-app

# Run migrations only and exit
docker run --env-file .env.production my-app ./app migrate

# Run the scheduler daemon
docker run --env-file .env.production my-app ./app schedule:work

# Run the queue worker
docker run --env-file .env.production my-app ./app queue:work
```

Pass production config through `--env-file .env.production` or
individual `-e` flags. `.env.production` should never be committed —
it's already covered by the `.dockerignore`.

### Bumping the Rust toolchain

The Dockerfile pins `rust:1.75-slim-bookworm` for the build stage so a
freshly-generated image is reproducible. Suprnova itself uses the 2024
edition and needs **Rust 1.85+**, so update the `FROM` line before the
first build:

```dockerfile
FROM rust:1.85-slim-bookworm AS backend-builder
```

Pin to whatever toolchain version matches what `rust-toolchain.toml` (if
you have one) or your local `rustc --version` reports.

### Why Suprnova diverges

Laravel deployments typically run **multiple processes per container or
host**: php-fpm for web, a queue worker, a scheduler, sometimes a
Horizon dashboard, sometimes an Octane runner. Each one is its own
service definition.

Suprnova compiles to **one statically-linked binary** that knows every
subcommand the framework ships — `serve`, `migrate`, `queue:work`,
`schedule:work`, `workflow:work`, `ssr:start`. The same Docker image
runs every role; the only thing that changes is the command. That makes
"web + worker + scheduler" three services in your orchestrator that all
point at the same image tag — one build to roll the entire app forward.

## docker:compose

Generate a `docker-compose.yml` that brings up local development
services.

```bash
suprnova docker:compose [OPTIONS]
```

Like `docker:init`, this refuses to overwrite an existing
`docker-compose.yml`. It also appends `docker-compose.override.yml` to
your `.gitignore` (if a `.gitignore` is present) so you can keep
per-developer overrides locally without committing them.

### Options

| Option | Description |
|--------|-------------|
| `--with-mailpit` | Include the Mailpit email-testing service |
| `--with-minio` | Include MinIO (S3-compatible object storage) |

If you pass neither flag, the command prompts interactively for both.
Passing either flag skips the prompt and uses the flag values you gave.

### What you always get

PostgreSQL and Redis are written into every generated compose file:

| Service | Default port | Image |
|---------|-------------:|-------|
| PostgreSQL | 5432 | `postgres:16-alpine` |
| Redis | 6379 | `redis:7-alpine` |

Both services have health checks, persistent named volumes, and live on
a project-scoped network (`<project>_network`). The Postgres user,
password, and database default to `suprnova` / `suprnova_secret` /
`suprnova_db`.

### Optional services

When you opt in:

| Service | Default ports | Image |
|---------|--------------:|-------|
| Mailpit | 1025 (SMTP), 8025 (UI) | `axllent/mailpit:latest` |
| MinIO | 9000 (S3 API), 9001 (Console) | `minio/minio:latest` |

Mailpit defaults to accepting any SMTP auth so you don't have to
configure credentials during development; the web UI at
`http://localhost:8025` shows every email your app sends. MinIO's
default credentials are `minioadmin` / `minioadmin`.

### Running the stack

```bash
# Bring everything up in the background
docker compose up -d

# Tail logs
docker compose logs -f

# Stop and remove the containers (volumes persist)
docker compose down

# Remove volumes too (wipes the local database)
docker compose down -v
```

### Wiring `.env` to compose

The compose file uses `${VAR:-default}` syntax everywhere, so you can
override anything by setting it in `.env` or your shell. A typical
`.env` for the default stack:

```env
DATABASE_URL=postgres://suprnova:suprnova_secret@localhost:5432/suprnova_db
REDIS_URL=redis://localhost:6379

# Mailpit (if enabled)
MAIL_DRIVER=smtp
MAIL_HOST=localhost
MAIL_PORT=1025

# MinIO (if enabled)
FILESYSTEM_DISK=s3
S3_ENDPOINT=http://localhost:9000
S3_ACCESS_KEY=minioadmin
S3_SECRET_KEY=minioadmin
S3_BUCKET=local
S3_REGION=us-east-1
```

To override a port (e.g. because 5432 is already in use), set the
matching env var before bringing the stack up:

```bash
DB_PORT=5433 docker compose up -d
```

The full set of overridable ports:

| Variable | Service | Default |
|----------|---------|--------:|
| `DB_PORT` | PostgreSQL | 5432 |
| `REDIS_PORT` | Redis | 6379 |
| `MAILPIT_SMTP_PORT` | Mailpit SMTP | 1025 |
| `MAILPIT_UI_PORT` | Mailpit UI | 8025 |
| `MINIO_API_PORT` | MinIO S3 | 9000 |
| `MINIO_CONSOLE_PORT` | MinIO Console | 9001 |

### Customising the compose file

`docker-compose.yml` is yours to edit after generation — Suprnova
doesn't regenerate or read it later. Common patches:

- Swap `postgres:16-alpine` for `mysql:8` or `mariadb:11` if you prefer
  one of those drivers; both are first-class in Suprnova
- Add a `volumes:` entry that mounts your `migrations/` directory if you
  want to run migrations inside a one-shot container
- Add additional services (Qdrant, Elasticsearch, Nats) the same way

## Production deployment

For a real deployment, run `docker:init` and treat the generated
`Dockerfile` as your build input. Most orchestrators (Railway, Fly,
Digital Ocean App Platform, Kubernetes) just need three things:

1. The image tag built from this `Dockerfile`
2. An env file with `DATABASE_URL`, `APP_KEY`, and any driver-specific
   keys
3. A health check pointing at `GET /_suprnova/health`

The single-binary shape means every role uses the same image; you
declare a "web" service running `./app` and a "scheduler" or "worker"
service running `./app schedule:work` (or `./app queue:work`). Both
read the same env, so they stay in lockstep on every deploy.

See [Deployment](deployment.md) for the platform-agnostic checklist,
and the platform guides for fully-worked examples:
[Railway](deployment-railway.md),
[Digital Ocean](deployment-digital-ocean.md),
[Hetzner VPS](deployment-hetzner.md).

## Summary

| Command | Writes | When to use |
|---------|--------|-------------|
| `suprnova docker:init` | `Dockerfile`, `.dockerignore` | Building production images |
| `suprnova docker:compose` | `docker-compose.yml` | Bringing up local Postgres/Redis/Mailpit/MinIO |

## Next

- [Deployment](deployment.md) — the platform-agnostic deployment checklist
- [Railway](deployment-railway.md) — managed PaaS with build-from-git
- [Digital Ocean](deployment-digital-ocean.md) — App Platform deploys
- [Hetzner VPS](deployment-hetzner.md) — bare-metal with systemd + Caddy
- [Environment Variables](env-vars.md) — every key the framework reads
