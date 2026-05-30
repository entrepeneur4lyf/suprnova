# Deploy to Hetzner VPS

This guide covers deploying a Suprnova application to a VPS using Hetzner Cloud. The same principles apply to any single-box host — Linode, Vultr, AWS EC2, or a dedicated server you already own. Choose this path when you want full control of the box, predictable monthly cost, and the ability to colocate Postgres / Redis on the same machine.

Throughout the guide we use `myapp` as the project name and `myapp.com` as the domain — substitute your own.

## Prerequisites

- A VPS running Ubuntu 22.04 or Debian 12
- SSH access to your server
- A domain name pointed to your server's IP address
- A Suprnova project — either a working source tree, or a Dockerfile generated with `suprnova docker:init` (see [Docker](cli-docker.md))

## Server Setup

### 1. Create a VPS

1. Go to [Hetzner Cloud Console](https://console.hetzner.cloud)
2. Create a new project and add a server
3. Choose **Ubuntu 22.04** as the image
4. Select your server size (CX11 is fine for small apps)
5. Add your SSH key for secure access

### 2. Initial Server Configuration

SSH into your server and run initial setup:

```bash
# Update packages
apt update && apt upgrade -y

# Create a non-root user for your app
useradd -m -s /bin/bash app
mkdir -p /opt/myapp
chown app:app /opt/myapp

# Install required packages
apt install -y curl postgresql redis-server
```

### 3. Configure PostgreSQL

```bash
# Create database and user
sudo -u postgres psql << EOF
CREATE USER myapp WITH PASSWORD 'your_secure_password';
CREATE DATABASE myapp_production OWNER myapp;
GRANT ALL PRIVILEGES ON DATABASE myapp_production TO myapp;
EOF
```

> **Tip:**
>
> For production, consider using a managed database service like Hetzner's upcoming managed PostgreSQL, or services like Neon, Supabase, or AWS RDS for better reliability and backups.


## Deploy Options

Choose one of the following deployment methods. Each one ends with a binary (or container) named `app` sitting at `/opt/myapp/app`, which the systemd unit below knows how to run.

### Option A: Build Locally

Build on your machine and upload the binary. Replace `myapp` with your actual project name — `cargo build` names the binary after the `[package].name` in `Cargo.toml`:

```bash
# On your local machine — cross-compile for Linux (if on macOS)
cargo build --release --target x86_64-unknown-linux-gnu

# Or build with Docker for Linux (the Dockerfile renames the binary to `app`)
docker build -t myapp .
docker create --name temp myapp
docker cp temp:/app/app ./app-linux
docker rm temp

# Upload to the server, renaming to `app` on landing
scp target/x86_64-unknown-linux-gnu/release/myapp root@your-server:/opt/myapp/app
# or, if you went the Docker route:
scp ./app-linux root@your-server:/opt/myapp/app
```

### Option B: Build on Server

Install Rust 1.85+ (Suprnova uses the 2024 edition) and build directly on the server:

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Clone, build, and place the binary at the standard path
cd /opt/myapp
git clone https://github.com/your-username/your-repo.git .
cargo build --release
cp target/release/myapp ./app   # rename so systemd's ExecStart=/opt/myapp/app finds it
```

### Option C: Use Docker

Run your app in a Docker container — the scaffolded Dockerfile already names the runtime binary `app` (see [Docker](cli-docker.md)):

```bash
# Install Docker
curl -fsSL https://get.docker.com | sh

# Pull and run your image
docker run -d \
  --name myapp \
  --restart unless-stopped \
  -p 8080:8080 \
  --env-file /opt/myapp/.env.production \
  your-registry/myapp:latest
```

If you went with Docker, skip past the systemd section to [Caddy Reverse Proxy](#caddy-reverse-proxy) — Docker handles process supervision.

## Environment Configuration

First, generate a production `APP_KEY` on the server (or locally — the value is what matters). `APP_KEY` is a 32-byte AES-256 key used by `suprnova::Crypt` for session cookies and signed URLs. Suprnova **fails closed at boot** when `APP_ENV` is not `local`/`dev`/`test` and `APP_KEY` is unset — so this is non-optional in production:

```bash
suprnova key:generate --show
# -> APP_KEY=base64-url-safe-32-bytes
```

Then write the env file:

```bash
cat > /opt/myapp/.env.production << 'EOF'
APP_NAME="My App"
APP_ENV=production
APP_DEBUG=false
APP_URL=https://myapp.com
APP_KEY=paste-the-generated-key-here

SERVER_HOST=127.0.0.1
SERVER_PORT=8080

# Database — bind to localhost when DB is on the same box
DATABASE_URL=postgres://myapp:your_secure_password@localhost:5432/myapp_production
DB_MAX_CONNECTIONS=10
DB_MIN_CONNECTIONS=1

# Session
SESSION_SECURE=true
SESSION_SAME_SITE=Lax

# Redis (optional — used by cache, queue, broadcasting drivers)
REDIS_URL=redis://127.0.0.1:6379

# Mail
MAIL_DRIVER=smtp
MAIL_HOST=your-smtp-host
MAIL_PORT=587
MAIL_USERNAME=
MAIL_PASSWORD=
MAIL_FROM_ADDRESS=hello@myapp.com
MAIL_FROM_NAME="My App"
EOF

# Secure the file — only the app user should be able to read it
chmod 600 /opt/myapp/.env.production
chown app:app /opt/myapp/.env.production
```

See [Configuration](configuration.md) for the full env surface and how it becomes typed config.

## systemd Services

A Suprnova binary supports multiple commands — `./app` (serve, with auto-migrate), `./app schedule:work` (scheduler daemon), `./app queue:work` (queue worker), `./app workflow:work` (workflow runner). Each long-running process gets its own systemd unit using the same binary and env file.

### Web Server Service

Create `/etc/systemd/system/myapp.service`:

```ini
[Unit]
Description=Suprnova Application
After=network.target postgresql.service redis.service
Requires=postgresql.service

[Service]
Type=simple
User=app
Group=app
WorkingDirectory=/opt/myapp
ExecStart=/opt/myapp/app
Restart=always
RestartSec=5

# Environment
EnvironmentFile=/opt/myapp/.env.production

# Security hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/opt/myapp

[Install]
WantedBy=multi-user.target
```

The default `ExecStart=/opt/myapp/app` runs `serve` with auto-migration. If you prefer migrations to be a separate deploy step, use `ExecStart=/opt/myapp/app serve --no-migrate` and run `./app migrate` from your deploy script before flipping the binary.

### Scheduler Service

If your app has tasks registered via `Schedule::call(...)` (see the [Scheduling](cli-scheduling.md) chapter), run **exactly one** scheduler process to avoid duplicate task execution. Create `/etc/systemd/system/myapp-scheduler.service`:

```ini
[Unit]
Description=Suprnova Scheduler
After=network.target myapp.service
Requires=myapp.service

[Service]
Type=simple
User=app
Group=app
WorkingDirectory=/opt/myapp
ExecStart=/opt/myapp/app schedule:work
Restart=always
RestartSec=5

# Environment
EnvironmentFile=/opt/myapp/.env.production

# Security hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/opt/myapp

[Install]
WantedBy=multi-user.target
```

### Queue Worker (optional)

If you dispatch jobs to a queue, add `/etc/systemd/system/myapp-queue.service`:

```ini
[Unit]
Description=Suprnova Queue Worker
After=network.target myapp.service
Requires=myapp.service

[Service]
Type=simple
User=app
Group=app
WorkingDirectory=/opt/myapp
ExecStart=/opt/myapp/app queue:work
Restart=always
RestartSec=5

EnvironmentFile=/opt/myapp/.env.production

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/opt/myapp

[Install]
WantedBy=multi-user.target
```

You can scale queue workers horizontally — multiple `myapp-queue.service` instances on the same or different boxes is safe.

### Enable and Start Services

```bash
# Reload systemd after writing unit files
systemctl daemon-reload

# Enable services so they start on boot
systemctl enable myapp
systemctl enable myapp-scheduler
systemctl enable myapp-queue        # if you added the queue worker

# Start them now
systemctl start myapp
systemctl start myapp-scheduler
systemctl start myapp-queue

# Verify
systemctl status myapp
systemctl status myapp-scheduler
systemctl status myapp-queue
```

## Caddy Reverse Proxy

Caddy automatically handles HTTPS certificates with Let's Encrypt.

### Install Caddy

```bash
apt install -y debian-keyring debian-archive-keyring apt-transport-https curl
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | tee /etc/apt/sources.list.d/caddy-stable.list
apt update
apt install caddy
```

### Configure Caddy

Edit `/etc/caddy/Caddyfile`:

```
myapp.com {
    reverse_proxy localhost:8080

    # Enable compression
    encode gzip

    # Logging
    log {
        output file /var/log/caddy/myapp.log
    }
}
```

Replace `myapp.com` with your actual domain.

### Start Caddy

```bash
systemctl enable caddy
systemctl start caddy
```

Caddy will automatically obtain and renew SSL certificates.

## Health Checks

Suprnova ships a built-in `/_suprnova/health` endpoint that short-circuits before the middleware chain and never collides with your routes:

```bash
curl https://myapp.com/_suprnova/health
```

```json
{
  "status": "ok",
  "timestamp": "2026-05-30T10:30:00Z"
}
```

### Check Database Connectivity

Add `?db=true` to also verify the database:

```bash
curl https://myapp.com/_suprnova/health?db=true
```

Healthy response (HTTP 200):

```json
{
  "status": "ok",
  "timestamp": "2026-05-30T10:30:00Z",
  "database": "connected"
}
```

If the database check fails, the endpoint flips to HTTP **503** with `"status": "degraded"` and a `"database_error"` field — wire this into a `livenessProbe` / `readinessProbe` style health check so the load balancer can remove an unhealthy instance from rotation.

### External Monitoring

Use the health endpoint with monitoring services:

- **UptimeRobot**: Add HTTP monitor for `https://myapp.com/_suprnova/health`
- **Better Stack** (formerly Better Uptime): Configure health check endpoint with the 503 trigger
- **Prometheus / Grafana**: Scrape the JSON body for `status` + `database` fields

## Deployment Script

Create a deployment script for atomic updates. Replace `myapp` with your project name (the `[package].name` in `Cargo.toml`) — that's what `cargo build` names the output binary:

```bash
#!/bin/bash
# deploy.sh - Run on your local machine

set -e

PROJECT="myapp"               # the Cargo package name
SERVER="root@your-server"
APP_PATH="/opt/myapp"
BIN="target/x86_64-unknown-linux-gnu/release/$PROJECT"

echo "Building application..."
cargo build --release --target x86_64-unknown-linux-gnu

echo "Uploading binary..."
scp "$BIN" "$SERVER:$APP_PATH/app.new"

echo "Deploying..."
ssh "$SERVER" << 'EOF'
    set -e
    cd /opt/myapp

    # Stop long-running services (ignore failures on first deploy)
    systemctl stop myapp-queue || true
    systemctl stop myapp-scheduler || true
    systemctl stop myapp

    # Atomic swap — rename is single-syscall on the same filesystem
    mv app.new app
    chmod +x app

    # Run migrations explicitly (the unit also auto-migrates, but doing
    # it here surfaces failures before we restart traffic)
    sudo -u app ./app migrate

    # Start services
    systemctl start myapp
    systemctl start myapp-scheduler || true
    systemctl start myapp-queue || true

    # Verify health (give the server a moment to bind)
    sleep 2
    curl -fsS http://localhost:8080/_suprnova/health?db=true > /dev/null || exit 1

    echo "Deployment complete!"
EOF
```

Make it executable:

```bash
chmod +x deploy.sh
./deploy.sh
```

## Logs and Monitoring

### View Logs

```bash
# Web server logs
journalctl -u myapp -f

# Scheduler logs
journalctl -u myapp-scheduler -f

# Caddy access logs
tail -f /var/log/caddy/myapp.log
```

### Log Rotation

systemd's journald handles log rotation automatically. For long-term storage, consider:

- **Loki + Grafana**: Self-hosted log aggregation
- **Papertrail**: Cloud-based logging service
- **Logtail**: Simple log management

## Firewall Configuration

Secure your server with UFW:

```bash
# Allow SSH
ufw allow 22/tcp

# Allow HTTP/HTTPS (Caddy)
ufw allow 80/tcp
ufw allow 443/tcp

# Enable firewall
ufw enable
```

> **Warning:**
>
> Never expose port 8080 directly. Always use Caddy as a reverse proxy to handle SSL and security headers.


## Scaling

A single Suprnova binary is very efficient — a small VPS handles a surprising amount of traffic before you need to scale out. When you do:

### Vertical Scaling

Upgrade the VPS to a larger instance for more CPU/memory. The binary, env file, and systemd units come with you unchanged.

### Horizontal Scaling

For multiple application instances:

1. Set up a load balancer (Hetzner Load Balancer, HAProxy, or Caddy on a dedicated node)
2. Move Postgres to a managed service or a dedicated node so app boxes are stateless
3. Move sessions, cache, and broadcasting to Redis so any app instance can serve any request
4. Deploy multiple app instances; each one safely runs its own auto-migrate on boot (the migration runner takes a lock so concurrent boots don't collide)
5. Keep **one** scheduler (`schedule:work`) running across the whole fleet — queue workers are safe to run in parallel, the scheduler isn't

### Why Suprnova diverges

Laravel typically runs PHP-FPM behind nginx, with cron triggering `schedule:run` once a minute and Horizon (or supervisord) managing queue workers. Suprnova collapses this into one binary with subcommands. `./app` is a long-lived Tokio process — it doesn't need a process pool in front of it, doesn't need a separate cron, and stays warm across requests. systemd is the supervisor for both the web process and the workers, and Caddy is doing only what nginx couldn't avoid: terminating TLS and proxying.

## Sizing

Pick a VPS based on workload, not on a marketing tier name. Hetzner's lineup changes periodically; the sizing logic doesn't:

| Workload | Rough fit |
|---|---|
| Small site, low traffic, SQLite or shared DB | Smallest shared-vCPU instance (1 vCPU / 2 GB) |
| Moderate traffic with Postgres + Redis on the same box | 2 vCPU / 4 GB |
| Heavier API + scheduler + queue workers + Postgres | 2–4 vCPU / 8 GB |
| Production at scale | Dedicated CPU instance, or split DB onto its own node |

Check Hetzner's [current pricing](https://www.hetzner.com/cloud) for the live catalogue. Suprnova's idle memory footprint is small (single-digit MB), so RAM is mostly database working set plus your domain code.

## Troubleshooting

### Service Won't Start

Check logs for errors:

```bash
journalctl -u myapp -n 50
```

Common issues:
- Missing environment variables
- Database connection failed
- Port already in use

### Caddy Certificate Errors

Ensure:
- Domain DNS points to your server
- Ports 80 and 443 are open
- No other service is using port 80

```bash
caddy validate --config /etc/caddy/Caddyfile
```

### Database Connection Issues

Test connection manually:

```bash
sudo -u app psql $DATABASE_URL -c "SELECT 1"
```

### Health Check Failing

```bash
# Check if app is running
systemctl status myapp

# Test health endpoint directly
curl http://localhost:8080/_suprnova/health

# Check with database
curl http://localhost:8080/_suprnova/health?db=true
```

A `503` response with `"status": "degraded"` means the app is up but the database health check failed — inspect `database_error` in the body and check the `DATABASE_URL`, Postgres logs, and connection limits.

## Next

- [Deployment Overview](deployment.md) — the platform-agnostic story for single-binary deploys
- [Docker](cli-docker.md) — `docker:init` and `docker:compose` details
- [Configuration](configuration.md) — full env surface and typed config
- [Deploy to Railway](deployment-railway.md) — PaaS alternative with automatic builds
- [Deploy to Digital Ocean](deployment-digital-ocean.md) — App Platform with managed infrastructure
