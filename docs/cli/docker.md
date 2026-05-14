---
title: 'Docker'
description: 'Generate Docker files for development and production deployment'
icon: 'docker'
---

suprnova provides commands to generate Docker configuration files for both local development and production deployment.

## docker:init

Generate a production-ready, multi-stage Dockerfile optimized for Rust applications.

```bash
suprnova docker:init
```

### What It Creates

| File | Purpose |
|------|---------|
| `Dockerfile` | Multi-stage build for production |
| `.dockerignore` | Excludes unnecessary files from build context |

### Dockerfile Features

The generated Dockerfile uses a multi-stage build for optimal image size:

1. **Stage 1: Frontend Build** - Builds the frontend assets using Node.js
2. **Stage 2: Backend Build** - Compiles the Rust application with release optimizations
3. **Stage 3: Runtime** - Minimal Debian-based image with only runtime dependencies

```dockerfile
# Stage 1: Build Frontend
FROM node:20-alpine AS frontend-builder
# ... builds frontend assets

# Stage 2: Build Rust Backend
FROM rust:1.75-slim-bookworm AS backend-builder
# ... compiles Rust with cargo build --release

# Stage 3: Runtime Image
FROM debian:bookworm-slim AS runtime
# ... minimal runtime with ca-certificates and libssl
```

### Building and Running

After generating the Dockerfile:

```bash
# Build the image
docker build -t my-app .

# Run the container
docker run -p 8080:8080 --env-file .env.production my-app
```

> **Tip:**
>
> Create a `.env.production` file with your production environment variables. Never commit this file to version control.


---

## docker:compose

Generate a `docker-compose.yml` file for local development with common services.

```bash
suprnova docker:compose [OPTIONS]
```

### Options

| Option | Description |
|--------|-------------|
| `--with-mailpit` | Include Mailpit email testing service |
| `--with-minio` | Include MinIO S3-compatible storage service |

### Default Services

PostgreSQL and Redis are always included:

| Service | Port | Description |
|---------|------|-------------|
| PostgreSQL | 5432 | Primary database |
| Redis | 6379 | Caching and session storage |

### Optional Services

When prompted (or using flags), you can add:

| Service | Ports | Description |
|---------|-------|-------------|
| Mailpit | 1025 (SMTP), 8025 (UI) | Local email testing |
| MinIO | 9000 (API), 9001 (Console) | S3-compatible object storage |

### Examples

```bash
# Interactive mode - prompts for optional services
suprnova docker:compose

# Include all optional services
suprnova docker:compose --with-mailpit --with-minio

# Include only email testing
suprnova docker:compose --with-mailpit
```

### Generated Configuration

The generated `docker-compose.yml` includes:

- **Environment variable defaults** - Uses `${VAR:-default}` syntax for easy customization
- **Health checks** - All services include health checks for reliability
- **Persistent volumes** - Data is preserved between container restarts
- **Custom network** - Services communicate on an isolated network

### Using the Services

Start all services:

```bash
docker compose up -d
```

Stop all services:

```bash
docker compose down
```

View logs:

```bash
docker compose logs -f
```

### Environment Configuration

Update your `.env` file to use the Docker services:

```env
# PostgreSQL
DATABASE_URL=postgres://suprnova:suprnova_secret@localhost:5432/suprnova_db

# Redis (if using caching)
REDIS_URL=redis://localhost:6379

# Mailpit (if enabled)
MAIL_HOST=localhost
MAIL_PORT=1025

# MinIO (if enabled)
S3_ENDPOINT=http://localhost:9000
S3_ACCESS_KEY=minioadmin
S3_SECRET_KEY=minioadmin
```

### Customizing Ports

Override default ports using environment variables:

```bash
# In your shell or .env file
DB_PORT=5433 docker compose up -d

# Or set them in .env
DB_PORT=5433
REDIS_PORT=6380
MAILPIT_SMTP_PORT=2025
MAILPIT_UI_PORT=8026
MINIO_API_PORT=9002
MINIO_CONSOLE_PORT=9003
```

### Service URLs

After starting, access services at:

| Service | URL |
|---------|-----|
| PostgreSQL | `localhost:5432` |
| Redis | `localhost:6379` |
| Mailpit UI | `http://localhost:8025` |
| MinIO Console | `http://localhost:9001` |

> **Note:**
>
> Mailpit captures all outgoing emails from your application. Access the web UI to view sent emails during development.


---

## Unified Binary Commands

Your suprnova application builds to a single binary with multiple commands:

```bash
./app                    # Run web server with auto-migrate (default)
./app serve              # Run web server with auto-migrate
./app serve --no-migrate # Run web server without migrations
./app migrate            # Run pending migrations
./app schedule:work      # Run scheduler daemon
```

When running in Docker:

```bash
# Default - runs web server with auto-migrations
docker run myapp

# Run scheduler
docker run myapp ./app schedule:work

# Run migrations only
docker run myapp ./app migrate
```

---

## Production Deployment

For production deployment:

1. **Generate Dockerfile**
   ```bash
   suprnova docker:init
   ```

2. **Build the image**
   ```bash
   docker build -t my-app .
   ```

3. **Run**
   ```bash
   docker run -d -p 8080:8080 --env-file .env.production my-app
   ```

> **Tip:**
>
> For detailed deployment guides, see [Deployment Overview](/deployment/overview), [Railway](/deployment/railway), or [Digital Ocean](/deployment/digital-ocean).


---

## Summary

| Command | Creates | Purpose |
|---------|---------|---------|
| `docker:init` | `Dockerfile`, `.dockerignore` | Production deployment |
| `docker:compose` | `docker-compose.yml` | Local development services |
