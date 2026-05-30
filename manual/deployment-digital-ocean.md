# Deploy to Digital Ocean

Digital Ocean has two production targets that suit a Suprnova app: **App
Platform** (a managed Docker PaaS — push and forget) and a **Droplet**
(your own VPS, you manage everything). This chapter walks through both.
Use App Platform when you want managed databases, automatic deploys,
and SSL handled for you. Use a Droplet when you want full control,
already run other services on the box, or want to keep the bill flat
regardless of traffic.

## Prerequisites

- A [Digital Ocean account](https://www.digitalocean.com)
- A Suprnova project with a Dockerfile — generate one with:
  ```bash
  suprnova docker:init
  ```
- An `APP_KEY` for production. Generate one and keep it somewhere safe:
  ```bash
  suprnova key:generate --show
  ```
  Suprnova fails closed on boot when `APP_ENV` is anything other than
  `local` / `development` / `testing` and `APP_KEY` is unset.
- A git repository (GitHub or GitLab) — required for App Platform; for
  Droplets you can also push a prebuilt image to a registry.

## App Platform

App Platform builds your Dockerfile, runs the single Suprnova binary,
and gives you a managed Postgres if you want one.

### 1. Create the app

1. Go to [Digital Ocean Apps](https://cloud.digitalocean.com/apps).
2. Click **Create App**, connect GitHub/GitLab, and pick the repo and
   branch.
3. App Platform auto-detects the `Dockerfile` at the repo root.

### 2. Configure the web service

| Setting | Value |
|---|---|
| Resource type | Web Service |
| HTTP port | `8080` |
| Run command | leave empty — the Dockerfile's `CMD` runs `./app` |
| Health check (HTTP path) | `/_suprnova/health` |

The default Suprnova binary runs `serve` with auto-migrations, so the
container will run migrations on startup and then bind the listener.

### 3. Add a managed Postgres

1. **Add Resource** -> **Database** -> **PostgreSQL**.
2. Pick a plan (Dev Database for testing; a Production plan for real
   traffic).

App Platform injects `DATABASE_URL` into every component automatically
via the `${db.DATABASE_URL}` binding.

### 4. Environment variables

In the **Environment Variables** section of your web component, set:

| Variable | Value | Notes |
|---|---|---|
| `APP_ENV` | `production` | triggers the fail-closed `APP_KEY` check |
| `APP_KEY` | output of `suprnova key:generate --show` | mark as **encrypted** |
| `SERVER_HOST` | `0.0.0.0` | bind to all interfaces |
| `SERVER_PORT` | `8080` | matches the Dockerfile's `EXPOSE` |
| `APP_URL` | `https://your-app.ondigitalocean.app` | used by Inertia + signed URLs |

`DATABASE_URL` is provided automatically by the managed database
binding; do not set it manually.

If you use Redis for cache/sessions, add a managed Redis cluster and
set `REDIS_URL` to its binding value (`${redis.REDIS_URL}`).

### 5. Deploy

Click **Create Resources**. The first build takes a few minutes
(Rust release build + frontend build); subsequent builds use the
Dockerfile layer cache and run much faster.

### Add a scheduler worker

Scheduled tasks (`#[derive(Task)]` handlers registered via
`Schedule::call`) need a long-lived process. Add a Worker component
that runs the same image with a different command:

1. **Create** -> **Add Resource** -> **Detect from source code**, select
   the same repository.
2. Set resource type to **Worker**.
3. **Run command**:
   ```bash
   ./app schedule:work
   ```
4. The worker inherits env vars from the app, including `DATABASE_URL`
   and `APP_KEY`.

Workers don't receive HTTP traffic. Run exactly **one** worker
instance — multiple schedulers would run each task multiple times.

For queue workers (`./app queue:work`) the pattern is identical;
you can usually run more than one queue worker safely because the
queue driver coordinates which worker takes which job. See
[Queues](queues.md).

### App spec (infrastructure as code)

For repeatable deploys, commit a `.do/app.yaml`:

```yaml
name: my-suprnova-app

services:
  - name: web
    dockerfile_path: Dockerfile
    github:
      repo: your-username/your-repo
      branch: main
      deploy_on_push: true
    http_port: 8080
    instance_count: 1
    instance_size_slug: basic-xxs
    health_check:
      http_path: /_suprnova/health
    envs:
      - key: APP_ENV
        value: production
      - key: APP_KEY
        scope: RUN_TIME
        type: SECRET
        value: ${APP_KEY}
      - key: SERVER_HOST
        value: 0.0.0.0
      - key: SERVER_PORT
        value: "8080"
      - key: APP_URL
        value: https://your-app.ondigitalocean.app
      - key: DATABASE_URL
        scope: RUN_TIME
        value: ${db.DATABASE_URL}

workers:
  - name: scheduler
    dockerfile_path: Dockerfile
    github:
      repo: your-username/your-repo
      branch: main
      deploy_on_push: true
    instance_count: 1
    instance_size_slug: basic-xxs
    run_command: ./app schedule:work
    envs:
      - key: APP_ENV
        value: production
      - key: APP_KEY
        scope: RUN_TIME
        type: SECRET
        value: ${APP_KEY}
      - key: DATABASE_URL
        scope: RUN_TIME
        value: ${db.DATABASE_URL}

databases:
  - name: db
    engine: PG
    version: "16"
    size: db-s-dev-database
```

Deploy with the `doctl` CLI:

```bash
doctl apps create --spec .do/app.yaml
```

Set the secret `APP_KEY` separately via the Apps UI or:

```bash
doctl apps update <app-id> --spec .do/app.yaml \
  --set-env "APP_KEY=$(suprnova key:generate --show)"
```

### Custom domain

In **Settings** -> **Domains** -> **Add Domain**, enter your domain and
follow the DNS instructions. App Platform issues and renews a
Let's Encrypt certificate automatically.

After the domain is live, update `APP_URL` to match — Inertia uses it
for the X-Inertia-Location header and signed URLs use it for the
hash input.

### Scaling

- **Horizontal**: bump **Instance Count** on the web service. Each
  instance shares the managed Postgres; multiple instances running
  auto-migrations on startup is safe — Suprnova uses SeaORM's
  advisory-locked migrator.
- **Vertical**: change **Instance Size**. The Rust binary is happy on
  the smallest slug for low-traffic apps; bump up when you start
  serving WebSockets or long-lived connections at scale.

Keep the scheduler worker at instance count **1**.

## Droplet (VPS)

A Droplet is the path when you want to run Suprnova on your own
VPS. The mechanics are identical to any other Linux VPS — systemd
service, Caddy reverse proxy, managed or self-hosted Postgres. The
[Hetzner VPS](deployment-hetzner.md) chapter is the canonical
walkthrough for that pattern; everything there applies verbatim on a
Droplet. The only differences worth calling out:

- **Image**: pick **Ubuntu 24.04** or **Debian 12** in the Droplet
  console.
- **Database**: you can use Digital Ocean's **Managed Databases** for
  Postgres / MySQL / Redis instead of running them on the Droplet —
  same `DATABASE_URL` / `REDIS_URL` story, point them at the managed
  endpoint and Suprnova doesn't notice the difference.
- **Backups**: enable Droplet snapshots and managed DB daily backups
  in the DO console.
- **Networking**: use a DO **VPC** to keep the Droplet and any managed
  databases on a private network; bind the listener to `127.0.0.1` and
  put Caddy in front for TLS.

If you want Docker on a Droplet (instead of a system binary), the
docker-compose pattern from [Docker](cli-docker.md) drops in cleanly —
swap the self-hosted Postgres for the managed database and you're done.

### Why Suprnova diverges

Laravel's typical PHP deploy needs PHP-FPM + an opcache + a queue
runner + a scheduler cron entry — at least three moving pieces, each
with its own restart semantics. A Suprnova deploy is a single binary
plus an optional worker process. The binary runs migrations, serves
HTTP, handles WebSockets, and lives behind a reverse proxy. The same
binary, invoked with `./app schedule:work` or `./app queue:work`, is
your scheduler or queue worker. App Platform's "one image, multiple
components" model fits this naturally — same Dockerfile for every
component, different `run_command` per role.

## Troubleshooting

### Build fails

The first thing to check is whether the Dockerfile builds locally:

```bash
docker build -t myapp .
```

Common causes when the local build works but App Platform's doesn't:

- **Missing build context files**: check `.dockerignore` isn't
  excluding `Cargo.lock` or the `migrations/` directory.
- **Out-of-memory during cargo build**: bump the build instance size
  in App Settings -> Resources -> Build. Rust release builds are
  memory-hungry.

### App boots, then crashes on startup

Check the runtime logs in the **Runtime Logs** tab. The two most
common Suprnova boot failures are:

- **`APP_KEY is required when APP_ENV=production`** — generate one with
  `suprnova key:generate --show` and add it as an encrypted env var.
- **`SERVER_HOST=…` value invalid** — must be `0.0.0.0` for App
  Platform, not `127.0.0.1` (the loopback isn't reachable from the
  load balancer).

### Health check failing

The platform pings `/_suprnova/health` and expects a 200 within the
configured timeout. If it's failing:

- Confirm the path is `/_suprnova/health` exactly (not `/health`).
- Confirm the port is `8080` and matches `SERVER_PORT`.
- Add `?db=true` to the health check path to also verify Postgres
  connectivity: `/_suprnova/health?db=true`. If this fails, the app
  can bind but can't reach Postgres — check the `DATABASE_URL`
  binding.

### Database migrations not running

Migrations run automatically as part of the default `./app` boot. If
they're not, check the runtime logs for SeaORM errors. To run them
manually from the App Platform console:

1. Open the **Console** tab on the web component.
2. Run `./app migrate`.

If you prefer to keep migrations out of the boot path, set the run
command to `./app serve --no-migrate` and add a one-shot **Job** to
the app spec that runs `./app migrate` pre-deploy.

## Next

- [Deployment Overview](deployment.md) — the cross-platform deploy
  primer (binary, migrations, scheduler, health)
- [Docker](cli-docker.md) — what `suprnova docker:init` and
  `docker:compose` generate
- [Configuration](configuration.md) — every env var Suprnova reads
- [Environment Variables](env-vars.md) — full reference, including
  the production-required ones
- [Deploy to Hetzner VPS](deployment-hetzner.md) — Droplet
  walkthrough applies here verbatim
