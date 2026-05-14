---
title: 'Deploy to Railway'
description: 'Deploy your suprnova application to Railway'
icon: 'train'
---

[Railway](https://railway.app) is a modern platform that makes deploying Rust applications easy. This guide walks you through deploying your suprnova application.

## Prerequisites

- A [Railway account](https://railway.app)
- Your suprnova project with a Dockerfile (suprnova `suprnova docker:init`)
- Git repository (GitHub, GitLab, or Bitbucket)

## Quick Start

### 1. Create a New Project

1. Go to [Railway Dashboard](https://railway.app/dashboard)
2. Click **"New Project"**
3. Select **"Deploy from GitHub repo"**
4. Connect your repository

### 2. Add PostgreSQL

1. In your project, click **"New"**
2. Select **"Database"** -> **"Add PostgreSQL"**
3. Railway creates the database and sets `DATABASE_URL` automatically

### 3. Configure Environment Variables

Click on your web service and go to **Variables**:

```env
APP_ENV=production
SERVER_HOST=0.0.0.0
SERVER_PORT=8080
```

> **Note:**
>
> Railway automatically injects `DATABASE_URL` from your PostgreSQL service. You don't need to set it manually.


### 4. Deploy

Railway automatically deploys when you push to your repository. Your suprnova application will:
1. Build using your Dockerfile
2. Run migrations on startup
3. Start serving requests

## Adding a Scheduler

If your application has scheduled tasks, add a worker service:

### 1. Add Another Service

1. In your project, click **"New"**
2. Select **"GitHub Repo"** (same repo)
3. Name it something like "scheduler"

### 2. Configure the Scheduler

In the scheduler service settings:

**Start Command:**
```bash
./app schedule:work
```

**Variables:** Copy the same environment variables from your web service, including `DATABASE_URL`.

> **Tip:**
>
> You can reference variables from other services using Railway's variable references: `${{Postgres.DATABASE_URL}}`


## Custom Domain

1. Go to your web service **Settings**
2. Click **"Generate Domain"** for a Railway subdomain
3. Or click **"Custom Domain"** to add your own

Railway handles SSL certificates automatically.

## Monitoring

Railway provides built-in monitoring:

- **Logs**: Real-time logs from your services
- **Metrics**: CPU, memory, and network usage
- **Deployments**: History and rollback options

## Example railway.json

For more control, add a `railway.json` to your project:

```json
{
  "$schema": "https://railway.app/railway.schema.json",
  "build": {
    "builder": "DOCKERFILE",
    "dockerfilePath": "Dockerfile"
  },
  "deploy": {
    "startCommand": "./app",
    "healthcheckPath": "/_suprnova/health",
    "healthcheckTimeout": 300,
    "restartPolicyType": "ON_FAILURE",
    "restartPolicyMaxRetries": 10
  }
}
```

## Costs

Railway offers:
- **Hobby Plan**: $5/month includes $5 of usage
- **Pro Plan**: $20/month with team features

Rust applications are typically very efficient, so costs remain low even under load.

## Troubleshooting

### Build Fails

Check that your Dockerfile builds locally:
```bash
docker build -t myapp .
```

### Connection Refused

Ensure `SERVER_HOST=0.0.0.0` is set. Railway requires binding to all interfaces.

### Migrations Fail

Check the DATABASE_URL is correct. View logs in the Railway dashboard for error details.

### Scheduler Not Running

Verify the start command is exactly `./app schedule:work` and that environment variables are configured.
