---
title: 'suprnova serve'
description: 'Start the development servers'
icon: 'play'
---

The `suprnova serve` command starts both the backend Rust server and the frontend Vite development server with hot-reload support.

## Usage

```bash
suprnova serve [options]
```

## Options

| Option | Default | Description |
|--------|---------|-------------|
| `-p, --port <PORT>` | `8000` | Backend server port |
| `--frontend-port <PORT>` | `5173` | Frontend Vite server port |
| `--backend-only` | `false` | Only start the backend server |
| `--frontend-only` | `false` | Only start the frontend server |
| `--skip-types` | `false` | Skip TypeScript type generation |

## Examples

### Start Both Servers

```bash
suprnova serve
```

Starts:
- Backend server on `http://localhost:8000`
- Frontend Vite server on `http://localhost:5173`
- Watches for changes and auto-rebuilds
- Generates TypeScript types from Rust structs

### Custom Ports

```bash
suprnova serve --port 3000 --frontend-port 3001
```

### Backend Only

```bash
suprnova serve --backend-only
```

Useful when:
- Working on API-only features
- Frontend is deployed separately
- Running in production mode

### Frontend Only

```bash
suprnova serve --frontend-only
```

Useful when:
- Backend is running elsewhere
- Testing frontend in isolation

### Skip Type Generation

```bash
suprnova serve --skip-types
```

Skips the automatic TypeScript type generation from Rust `InertiaProps` structs. Useful if you're managing types manually.

## How It Works

When you run `suprnova serve`, the CLI:

1. **Builds the Rust backend** - Compiles your application in debug mode
2. **Generates TypeScript types** - Scans for `InertiaProps` structs and generates corresponding TypeScript interfaces
3. **Starts the backend server** - Runs your Rust application with hot-reload using `cargo watch`
4. **Starts the frontend server** - Runs Vite development server with HMR (Hot Module Replacement)

## Development Workflow

```bash
# Terminal 1: Start the full development environment
suprnova serve

# Make changes to your Rust code - backend auto-rebuilds
# Make changes to your React code - frontend auto-updates

# Terminal 2: Generate migrations, run them
suprnova make:migration create_users_table
suprnova migrate
```

## Environment Variables

The serve command respects environment variables from `.env`:

```env
# .env
DATABASE_URL=sqlite:./database.db
APP_PORT=8000
APP_HOST=127.0.0.1
```

## Troubleshooting

### Port Already in Use

If you see a port conflict error:

```bash
# Find and kill the process using the port
lsof -i :8000
kill -9 <PID>

# Or use a different port
suprnova serve --port 8001
```

### Frontend Not Connecting

Ensure your frontend is configured to proxy to the correct backend port in `vite.config.ts`.
