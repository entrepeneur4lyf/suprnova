# Deploy to Digital Ocean

[Digital Ocean App Platform](https://www.digitalocean.com/products/app-platform) is a Platform-as-a-Service that supports Docker deployments. This guide walks you through deploying your suprnova application.

## Prerequisites

- A [Digital Ocean account](https://www.digitalocean.com)
- Your suprnova project with a Dockerfile (suprnova `suprnova docker:init`)
- Git repository (GitHub or GitLab)

## Quick Start

### 1. Create a New App

1. Go to [Digital Ocean Apps](https://cloud.digitalocean.com/apps)
2. Click **"Create App"**
3. Connect your GitHub/GitLab account
4. Select your repository

### 2. Configure the App

Digital Ocean will detect your Dockerfile. Configure the settings:

**Resource Type:** Web Service

**HTTP Port:** 8080

**Run Command:** Leave empty (uses Dockerfile CMD)

### 3. Add a Database

1. Click **"Add Resource"**
2. Select **"Database"** -> **"PostgreSQL"**
3. Choose your plan (Dev Database for testing, or Production)

Digital Ocean creates a `DATABASE_URL` environment variable automatically.

### 4. Set Environment Variables

In the **Environment Variables** section, add:

| Variable | Value |
|----------|-------|
| `APP_ENV` | production |
| `SERVER_HOST` | 0.0.0.0 |
| `SERVER_PORT` | 8080 |

> **Note:**
>
> `DATABASE_URL` is set automatically when you add a managed database.


### 5. Deploy

Click **"Create Resources"** to deploy. Your app will:
1. Build from Dockerfile
2. Run migrations on startup
3. Start serving requests

## Adding a Scheduler (Worker)

For scheduled tasks, add a Worker component:

### 1. Add a Worker

1. In your app, click **"Create"** -> **"Add Resource"**
2. Select **"Detect from source code"**
3. Choose the same repository
4. Set resource type to **"Worker"**

### 2. Configure the Worker

**Run Command:**
```bash
./app schedule:work
```

**Environment Variables:** The worker inherits variables from the app, including `DATABASE_URL`.

> **Tip:**
>
> Workers don't receive HTTP traffic and are ideal for background processes like the scheduler.


## App Spec (Optional)

For infrastructure-as-code, create `.do/app.yaml`:

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
    envs:
      - key: APP_ENV
        value: production
      - key: SERVER_HOST
        value: 0.0.0.0
      - key: SERVER_PORT
        value: "8080"
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
      - key: DATABASE_URL
        scope: RUN_TIME
        value: ${db.DATABASE_URL}

databases:
  - name: db
    engine: PG
    version: "16"
    size: db-s-dev-database
```

Deploy with:
```bash
doctl apps create --spec .do/app.yaml
```

## Custom Domain

1. Go to your app **Settings**
2. Click **"Domains"** -> **"Add Domain"**
3. Enter your domain name
4. Update your DNS records as instructed

Digital Ocean provides free SSL certificates.

## Scaling

### Horizontal Scaling

In your app settings, increase the **Instance Count** for your web service. Each instance runs independently with the same database.

### Vertical Scaling

Upgrade your **Instance Size** for more CPU and memory:

| Size | vCPU | Memory | Cost/mo |
|------|------|--------|---------|
| basic-xxs | 1 | 512MB | $5 |
| basic-xs | 1 | 1GB | $10 |
| basic-s | 1 | 2GB | $20 |
| basic-m | 2 | 4GB | $40 |

## Monitoring

Digital Ocean provides:

- **Logs**: Real-time container logs
- **Metrics**: CPU, memory, bandwidth
- **Alerts**: Set up notifications for issues

## Costs

- **App Platform**: Starts at $5/month for basic apps
- **Managed PostgreSQL**: Starts at $15/month (Dev database $7/month)
- **Bandwidth**: 1TB included, then $0.10/GB

## Troubleshooting

### Build Fails

Check the build logs. Common issues:
- Missing Dockerfile
- Dependency compilation errors

Test locally:
```bash
docker build -t myapp .
```

### App Won't Start

Verify `SERVER_HOST=0.0.0.0`. Digital Ocean requires binding to all interfaces.

### Database Connection Failed

Ensure:
- Database is in "available" state
- `DATABASE_URL` environment variable is set
- No typos in the connection string

### Migrations Not Running

Migrations run automatically with the default command. Check logs for errors. To run manually:

1. Open the **Console** tab
2. Run: `./app migrate`
