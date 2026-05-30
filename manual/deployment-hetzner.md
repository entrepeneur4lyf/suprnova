# Deploy to Hetzner VPS

This guide covers deploying your suprnova application to a VPS (Virtual Private Server) using Hetzner Cloud. The same principles apply to other VPS providers like DigitalOcean Droplets, Linode, Vultr, or AWS EC2.

## Prerequisites

- A VPS running Ubuntu 22.04 or Debian 12
- SSH access to your server
- A domain name pointed to your server's IP address
- Your suprnova project with a Dockerfile (suprnova `suprnova docker:init`)

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

Choose one of the following deployment methods:

**Option A: Build Locally**

Build on your local machine and upload the binary:

    ```bash
    # On your local machine
    # Cross-compile for Linux (if on macOS)
    cargo build --release --target x86_64-unknown-linux-gnu

    # Or build with Docker for Linux
    docker build -t myapp .
    docker create --name temp myapp
    docker cp temp:/app/app ./app-linux
    docker rm temp

    # Upload to server
    scp ./app-linux root@your-server:/opt/myapp/app
    ```

**Option B: Build on Server**

Install Rust and build directly on the server:

    ```bash
    # Install Rust
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    source ~/.cargo/env

    # Clone and build
    cd /opt/myapp
    git clone https://github.com/your-username/your-repo.git .
    cargo build --release
    cp target/release/app ./app
    ```

**Option C: Use Docker**

Run your app in a Docker container:

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

    Skip to the [Caddy Setup](#caddy-reverse-proxy) section if using Docker.

## Environment Configuration

Create your production environment file:

```bash
cat > /opt/myapp/.env.production << 'EOF'
APP_ENV=production
SERVER_HOST=127.0.0.1
SERVER_PORT=8080

# Database
DATABASE_URL=postgres://myapp:your_secure_password@localhost:5432/myapp_production

# Redis (optional)
REDIS_URL=redis://127.0.0.1:6379

# Your app-specific variables
APP_KEY=your-secure-app-key
EOF

# Secure the file
chmod 600 /opt/myapp/.env.production
chown app:app /opt/myapp/.env.production
```

## systemd Services

### Web Server Service

Create `/etc/systemd/system/myapp.service`:

```ini
[Unit]
Description=suprnova Application
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

### Scheduler Service

If your app has scheduled tasks, create `/etc/systemd/system/myapp-scheduler.service`:

```ini
[Unit]
Description=suprnova Scheduler
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

### Enable and Start Services

```bash
# Reload systemd
systemctl daemon-reload

# Enable services (start on boot)
systemctl enable myapp
systemctl enable myapp-scheduler

# Start services
systemctl start myapp
systemctl start myapp-scheduler

# Check status
systemctl status myapp
systemctl status myapp-scheduler
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

suprnova includes a built-isuprnova/_suprnova/health` endpoint that returns:

```json
{
  "status": "ok",
  "timestamp": "2024-12-28T10:30:00Z"
}
```

### Check Database Connectivity

Add `?db=true` to also verify database connectivity:

```bash
curl https://myapp.com/_suprnova/health?db=true
```

Returns:

```json
{
  "status": "ok",
  "timestamp": "2024-12-28T10:30:00Z",
  "database": "connected"
}
```

### External Monitoring

Use the health endpoint with monitoring services:

- **UptimeRobot**: Add HTTP monitor for `https://myapp.com/_suprnova/health`
- **Better Uptime**: Configure health check endpoint
- **Grafana**: Scrape health endpoint metrics

## Deployment Script

Create a deployment script for easy updates:

```bash
#!/bin/bash
# deploy.sh - Run on your local machine

set -e

SERVER="root@your-server"
APP_PATH="/opt/myapp"

echo "Building application..."
cargo build --release --target x86_64-unknown-linux-gnu

echo "Uploading binary..."
scp target/x86_64-unknown-linux-gnu/release/app $SERVER:$APP_PATH/app.new

echo "Deploying..."
ssh $SERVER << 'EOF'
    cd /opt/myapp

    # Stop services
    systemctl stop myapp-scheduler || true
    systemctl stop myapp

    # Replace binary
    mv app.new app
    chmod +x app

    # Run migrations
    sudo -u app ./app migrate

    # Start services
    systemctl start myapp
    systemctl start myapp-scheduler

    # Verify health
    sleep 2
    curl -f http://localhost:8080/_suprnova/health || exit 1

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

### Vertical Scaling

Upgrade your VPS to a larger instance for more CPU/memory.

### Horizontal Scaling

For multiple instances:

1. Set up a load balancer (Hetzner Load Balancer or HAProxy)
2. Use a managed database (external PostgreSQL)
3. Use Redis for session storage
4. Deploy multiple app instances behind the load balancer

## Costs

Hetzner offers competitive pricing:

| Instance | vCPU | RAM | Storage | Monthly |
|----------|------|-----|---------|---------|
| CX11 | 1 | 2GB | 20GB | ~$4 |
| CX21 | 2 | 4GB | 40GB | ~$6 |
| CX31 | 2 | 8GB | 80GB | ~$12 |
| CX41 | 4 | 16GB | 160GB | ~$22 |

Plus managed PostgreSQL when available, or use external services.

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
