# Deployment Overview

suprnova applications compile to a single, self-contained binary that includes everything needed to run your application: web server, migrations, and scheduler.

## Single Binary Architecture

Your suprnova application builds to one binary with multiple commands:

```bash
./app                    # Run web server with auto-migrate (default)
./app serve              # Run web server with auto-migrate
./app serve --no-migrate # Run web server without migrations
./app migrate            # Run pending migrations
./app migrate:status     # Show migration status
./app migrate:rollback   # Rollback last migration
./app migrate:fresh      # Drop all tables and re-run migrations
./app schedule:work      # Run scheduler daemon
./app schedule:run       # Run due tasks once
./app schedule:list      # List registered tasks
```

This architecture simplifies deployment:
- One Docker image for all services
- Same binary for web server and scheduler
- Migrations run automatically on startup

## Building for Production

### Generate Dockerfile

```bash
suprnova docker:init
```

This creates a multi-stage Dockerfile optimized for production:

1. **Frontend build** - Compiles React/TypeScript assets
2. **Backend build** - Compiles Rust with release optimizations
3. **Runtime** - Minimal Debian image with only runtime dependencies

### Build the Image

```bash
docker build -t myapp .
```

### Test Locally

```bash
# Run with environment file
docker run -p 8080:8080 --env-file .env.production myapp

# Or pass environment variables directly
docker run -p 8080:8080 \
  -e DATABASE_URL=postgres://user:pass@host:5432/db \
  -e APP_ENV=production \
  myapp
```

## Environment Variables

Configure your application with these environment variables:

| Variable | Description | Default |
|----------|-------------|---------|
| `DATABASE_URL` | PostgreSQL connection string | Required |
| `APP_ENV` | Environment (development/production) | development |
| `SERVER_HOST` | Host to bind to | 0.0.0.0 |
| `SERVER_PORT` | Port to listen on | 8080 |
| `REDIS_URL` | Redis connection string (if using cache) | Optional |

### Example .env.production

```env
DATABASE_URL=postgres://user:password@host:5432/myapp_production
APP_ENV=production
SERVER_HOST=0.0.0.0
SERVER_PORT=8080
REDIS_URL=redis://redis-host:6379
```

> **Warning:**
>
> Never commit `.env.production` to version control. Add it to your `.gitignore`.


## Migrations

By default, the web server runs migrations automatically on startup. This ensures your database schema is always up to date.

### Behavior

- **Default (`./app`)**: Runs migrations silently, then starts server
- **Explicit (`./app serve`)**: Same as default
- **Skip migrations (`./app serve --no-migrate`)**: Start server without migrations
- **Migrations only (`./app migrate`)**: Run migrations and exit

### In Production

For most deployments, the default auto-migrate behavior is ideal:

```bash
# The container will:
# 1. Connect to the database
# 2. Run any pending migrations
# 3. Start the web server
docker run myapp
```

If you need to run migrations separately (e.g., in a CI/CD pipeline):

```bash
# Run migrations only
docker run myapp ./app migrate

# Then start the server without migrations
docker run myapp ./app serve --no-migrate
```

## Running the Scheduler

If your application has scheduled tasks, run the scheduler as a separate process:

```bash
# Run the scheduler daemon
docker run myapp ./app schedule:work
```

The scheduler will:
- Check for due tasks every minute
- Run tasks in the background
- Log task execution and errors

### Deployment Pattern

Most platforms support running multiple services from the same Docker image:

1. **Web service**: `./app` (default command)
2. **Scheduler service**: `./app schedule:work`

Both services use the same image and environment variables.

## Health Checks

suprnova includes a built-isuprnova/_suprnova/health` endpoint that returns JSON status information. Thsuprnova/_suprnova` prefix ensures it never conflicts with your application routes.

```bash
curl http://localhost:8080/_suprnova/health
```

Response:

```json
{
  "status": "ok",
  "timestamp": "2024-12-28T10:30:00Z"
}
```

### Database Health Check

Add `?db=true` to verify database connectivity:

```bash
curl http://localhost:8080/_suprnova/health?db=true
```

Response:

```json
{
  "status": "ok",
  "timestamp": "2024-12-28T10:30:00Z",
  "database": "connected"
}
```

### Platform Configuration

Configure your platform's health check to:

- **Path**: `/_suprnova/health`
- **Port**: 8080 (or `SERVER_PORT`)
- **Protocol**: HTTP

## Scaling

### Horizontal Scaling (Web)

Scale your web servers horizontally by running multiple instances behind a load balancer. Each instance:
- Runs auto-migrations on startup (safe with multiple instances)
- Serves HTTP requests independently
- Shares the same database

### Scheduler

Run only **one** scheduler instance to avoid duplicate task execution. Most platforms support marking a service as a "worker" that only runs one instance.

## Next Steps

Choose your deployment platform:

- [Deploy to Railway](deployment-railway.md) - Simple PaaS with automatic builds
- [Deploy to Digital Ocean](deployment-digital-ocean.md) - App Platform with managed infrastructure
- [Deploy to Hetzner VPS](deployment-hetzner.md) - Full control with systemd and Caddy
