# Deployment Overview

A Suprnova app compiles to a single self-contained binary that owns
the web server, the migration runner, the scheduler, and the queue
worker. Deploying is "copy the binary, set four environment variables,
run it." This chapter covers what those four variables are, what the
binary's subcommands do in production, and how the built-in health
endpoint integrates with a platform's liveness probe. Platform-specific
walkthroughs follow in [Railway](deployment-railway.md),
[Digital Ocean](deployment-digital-ocean.md), and
[Hetzner](deployment-hetzner.md).

## The single binary

Your app builds to one binary with a clap subcommand surface:

```bash
./app                       # serve (default) — auto-migrate, then HTTP
./app serve                 # explicit serve, with auto-migrate
./app serve --no-migrate    # serve without running migrations
./app web:run               # alias for serve

./app migrate               # apply pending migrations and exit
./app migrate:status        # show migration status
./app migrate:rollback [N]  # roll back the last N migrations (default 1)
./app migrate:fresh         # drop all tables, then re-migrate

./app schedule:work         # scheduler daemon — wakes every minute
./app schedule:run          # run due tasks once and exit
./app schedule:list         # print every registered task
./app queue:work            # queue worker daemon
./app workflow:work         # workflow worker daemon

./app down [--secret …] [--retry …] [--except …] [--message …]
./app up                    # leave maintenance mode
```

One binary means one Docker image, one CI artifact, one deployment to
verify. The same image runs the web service, the scheduler, the queue
worker, and the workflow worker — you start a different subcommand
for each.

## Four production environment variables

Suprnova fails closed on boot if the production environment is
misconfigured. The minimum set to deploy:

| Variable | What it does | Failure mode |
|---|---|---|
| `APP_ENV` | Selects the environment (`production`, `staging`, etc.). | Defaults to `local` if unset — your app runs in dev mode in prod. |
| `APP_KEY` | 32-byte AES-256 base64 key for `Crypt`, sessions, cookies, and pagination cursors. | Boot returns a typed error and exits non-zero when `APP_ENV` is not local/dev/test and `APP_KEY` is missing or malformed. |
| `APP_URL` | Canonical absolute URL of your app (`https://app.example.com`). | Defaults to `http://localhost:8765`; signed URLs, redirects, mail links, and absolute Inertia URLs all use this. |
| `DATABASE_URL` | Connection URL for your relational database. | Boot refuses to start when `APP_ENV` is `production` or `staging` and `DATABASE_URL` is unset — the dev SQLite fallback is rejected explicitly. |

Generate `APP_KEY` once with the CLI:

```bash
suprnova key:generate           # writes APP_KEY=… into ./.env
suprnova key:generate --show    # prints the key for $(…)
```

For key rotation, see [Encryption](encryption.md) —
`APP_KEY_PREVIOUS` (or the Laravel-compatible `APP_PREVIOUS_KEYS`)
takes a comma-separated list of older keys for decrypt-only fallback.

Beyond the four required vars, common production knobs:

| Variable | Default | Notes |
|---|---|---|
| `SERVER_HOST` | `127.0.0.1` | Use `0.0.0.0` in containers. |
| `SERVER_PORT` | `8765` | Match your platform's expected port. |
| `APP_DEBUG` | env-derived | `false` in production/staging/custom envs. Set explicitly if you want loud errors in staging. |
| `SERVER_MAX_BODY_SIZE` | per-handler default | Process-wide request body cap. |
| `DB_MAX_CONNECTIONS` | `10` | Pool size. |
| `REDIS_URL` | unset | Required if you've configured the Redis cache/queue/session drivers. |

The full table lives in [Environment Variables](env-vars.md).

## Recommended database: MariaDB

Suprnova supports SQLite, PostgreSQL, MySQL, and MariaDB as first-class
relational backends. The recommendation is environment-specific:

- **Development.** SQLite. The scaffolder writes
  `DATABASE_URL=sqlite://./database.db` so `suprnova serve` works
  with zero database setup.
- **Production.** MariaDB. It collapses what would otherwise be three
  separate services (relational + vector + KV cache) onto one engine,
  with system-versioned tables for audit if you need them.

```bash
# .env.production
DATABASE_URL=mysql://app_user:secret@db.internal:3306/app_production
```

Use the `mysql://` scheme — SeaORM's MySQL driver handles MariaDB
natively, and Suprnova's `MariaDbVectorDriver` (`VECTOR(N)` + HNSW)
hooks in directly for vector workloads.

The other relational backends are first-class too:

```bash
# PostgreSQL
DATABASE_URL=postgres://app_user:secret@db.internal:5432/app_production

# MySQL
DATABASE_URL=mysql://app_user:secret@db.internal:3306/app_production

# SQLite (for tiny single-instance deploys)
DATABASE_URL=sqlite:///var/lib/myapp/data.db
```

### Why Suprnova diverges

Laravel's defaults nudge new projects toward PostgreSQL because PHP +
PostgreSQL is the well-trodden path. Suprnova picks the database that
gives the cleanest single-engine production posture for a Rust app.
MariaDB's `VECTOR(N)` (11.7+), Dynamic Columns, and system-versioned
tables mean a small-to-mid product can ship search, KV, and audit
without bolting on Redis, OpenSearch, or pgvector. PostgreSQL stays
fully supported — the framework's test matrix runs against all three
relational backends — but our deployment docs lead with the engine
that minimises moving parts. See
[Vector Storage](vector.md) and [Database](database.md) for the
backend-specific surfaces.

## Building a production image

The scaffolder ships a generator for a multi-stage Dockerfile:

```bash
suprnova docker:init
```

This writes a `Dockerfile` with three stages:

1. **Frontend build** — `node:20-alpine`, runs `npm ci && npm run build`
   against your `frontend/` Inertia app (Svelte 5, React 19, or Vue 3.5
   per your scaffold choice).
2. **Backend build** — `rust:slim-bookworm`, compiles your crate in
   release mode with dependency caching.
3. **Runtime** — `debian:bookworm-slim`, copies the compiled binary
   and Vite output, runs as a non-root `appuser`, exposes port 8765,
   and runs `CMD ["./app"]` (the auto-migrating server).

Build and run locally to verify before pushing:

```bash
docker build -t myapp .

# With an env file
docker run --rm -p 8765:8765 --env-file .env.production myapp

# Or with explicit vars (the four required ones)
docker run --rm -p 8765:8765 \
  -e APP_ENV=production \
  -e APP_KEY=$APP_KEY \
  -e APP_URL=https://app.example.com \
  -e DATABASE_URL=mysql://user:pass@host:3306/app \
  myapp
```

Never commit `.env.production` (or any file containing `APP_KEY` or
`DATABASE_URL`) to your repo. Use your platform's secrets store and
read the values at deploy time.

## Migrations on boot

The default `./app` (and explicit `./app serve`) command applies any
pending migrations before binding the socket. The two practical
implications:

- **Safe with multiple instances.** SeaORM's migration runner takes a
  database-level advisory lock; the slowest pod waits, the others
  proceed once it's done. You do not need a separate "migrate-then-deploy"
  step for routine release rolls.
- **Failed migration = failed deploy.** If a migration errors, the
  process exits non-zero before the server binds. The platform's
  health probe (see below) reports the pod unhealthy, and the rollout
  halts. Fix forward by shipping a corrective migration in the next
  release.

For CI pipelines that want to gate the deploy on a successful migration
before any pod accepts traffic, run migrations in a one-shot:

```bash
docker run --rm myapp ./app migrate
# … then roll the actual deploy
docker run myapp ./app serve --no-migrate
```

`--no-migrate` skips the auto-migrate phase but still boots the server
normally.

## Workers as separate services

The scheduler, queue, and workflow systems each have their own daemon
subcommand. In production, run them as separate processes against the
same image, sharing the same environment:

```bash
docker run myapp ./app schedule:work    # one instance — see below
docker run myapp ./app queue:work       # scale to N instances
docker run myapp ./app workflow:work    # scale to N instances
```

Two rules to internalise:

- **Run exactly one `schedule:work` process.** Multiple scheduler
  instances would dispatch every cron tick more than once. Most
  platforms model this as a "worker" service with `replicas: 1`.
- **Queue and workflow workers scale horizontally.** Both pull work
  from a shared store and use visibility timeouts or row-level locks
  to coordinate; adding pods adds throughput. `./app queue:work
  --max-jobs N` makes the worker exit after N jobs so a supervisor can
  rotate the process — useful for release-on-restart deploys.

See [Queues](queues.md), [Scheduling](scheduling.md), and
[Workflows](workflows.md) for the per-subsystem detail.

## Health check

Suprnova exposes a built-in liveness endpoint at `/_suprnova/health`.
The `_suprnova/` prefix is reserved so your own routes can never
collide with it.

```bash
curl http://localhost:8765/_suprnova/health
# {"status":"ok","timestamp":"2026-05-30T12:34:56+00:00"}
```

Add `?db=true` to also probe the database:

```bash
curl http://localhost:8765/_suprnova/health?db=true
# Healthy:
#   200 {"status":"ok","timestamp":"…","database":"connected"}
# Degraded:
#   503 {"status":"degraded","timestamp":"…","database":"error","database_error":"…"}
```

The status flips to HTTP 503 when any sub-check fails, so a Kubernetes
`livenessProbe` / `readinessProbe`, a Railway healthcheck, or a
Digital Ocean health check can wire up directly:

```yaml
livenessProbe:
  httpGet:
    path: /_suprnova/health
    port: 8765
readinessProbe:
  httpGet:
    path: /_suprnova/health?db=true
    port: 8765
```

The endpoint short-circuits before the middleware chain so it stays
responsive even if a middleware deadlocks or the request id middleware
is rejecting traffic.

## Maintenance mode

To roll a destructive migration or quiesce traffic for an incident:

```bash
./app down --secret abc123 \
           --retry 60 \
           --message "Deploying — back in a few minutes" \
           --except /webhooks/stripe

./app up
```

`down` writes a maintenance marker the middleware reads on every
request. Requests get a 503 (configurable via `--status`) with the
provided message, except for paths in `--except` and any request that
includes the secret. `up` removes the marker.

## Scaling

### Web

Horizontal scaling is the default story: every pod runs `./app`,
shares `DATABASE_URL`, and connects to the same Redis (if you've
configured Redis-backed cache/queue/session). Auto-migration is safe
because of the advisory lock above. Sticky sessions are not required
— session state lives in your session driver (database or Redis),
not in process memory.

### Workers

- **Scheduler.** Exactly one instance, always.
- **Queue.** Scale horizontally. If you've split work across multiple
  named queues, run a worker per queue (or pass driver-specific queue
  filters — see [Queues](queues.md)).
- **Workflow.** Scale horizontally; row-level claim/heartbeat
  coordinates the workers.

## Per-platform walkthroughs

The recipe above ports to every modern PaaS or VPS. The next three
chapters walk you through the specifics:

| Platform | Style | Walkthrough |
|---|---|---|
| Railway | PaaS with auto-deploy from git | [Deploy to Railway](deployment-railway.md) |
| Digital Ocean | App Platform (PaaS) or Droplets (VPS) | [Deploy to Digital Ocean](deployment-digital-ocean.md) |
| Hetzner | VPS with systemd + Caddy | [Deploy to Hetzner](deployment-hetzner.md) |

## Next

- [Environment Variables](env-vars.md) — every env var the framework reads
- [Encryption](encryption.md) — `APP_KEY`, key rotation, what's encrypted
- [Configuration](configuration.md) — typed config sections built on top of env
- [Database](database.md) — driver selection, pool tuning, multi-connection split
- [Queues](queues.md) — worker scaling and queue drivers
