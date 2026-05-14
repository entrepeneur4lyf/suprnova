---
title: 'Development'
description: 'Development workflow and tools for building suprnova applications'
icon: 'code'
---

This guide covers the development workflow for building suprnova applications, including the dev server, hot reloading, and useful commands.

## Development Server

Start the development server with:

```bash
suprnova serve
```

This starts:
- **Backend server** on `http://localhost:8080`
- **Vite dev server** on `http://localhost:5173` (for frontend assets)

### Backend Only Mode

If you only need the Rust backend (no frontend):

```bash
suprnova serve --backend-only
```

### Custom Ports

Configure ports using environment variables:

```env
# .env
PORT=3000
VITE_DEV_SERVER=http://localhost:5174
```

## Project Structure

A typical suprnova project structure:

```
my-app/
├── src/
│   ├── main.rs           # Application entry point
│   ├── routes.rs         # Route definitions
│   ├── controllers/      # Request handlers
│   ├── models/           # Database entities
│   ├── actions/          # Business logic
│   └── middleware/       # Custom middleware
├── frontend/
│   ├── src/
│   │   ├── main.tsx      # Frontend entry
│   │   └── pages/        # React components
│   ├── package.json
│   └── vite.config.ts
├── migrations/           # Database migrations
├── .env                  # Environment config
└── Cargo.toml
```

## Development Workflow

### 1. Make Changes

Edit your Rust code in `src/` or React code in `frontend/src/`.

### 2. Automatic Recompilation

suprnova watches for changes:
- **Rust changes**: Recompiles automatically
- **React changes**: Hot module replacement (instant updates)

### 3. Test Your Changes

Visit `http://localhost:8080` to see your changes.

## Database Development

### Running Migrations

```bash
# Run all pending migrations
suprnova migrate

# Check migration status
suprnova migrate:status

# Rollback last migration
suprnova migrate:rollback
```

### Syncing Entities

After modifying migrations, regenerate entity files:

```bash
suprnova db:sync
```

### Fresh Database

Reset and re-run all migrations:

```bash
suprnova migrate:fresh
```

## Code Generation

suprnova provides generators to scaffold common components:

```bash
# Create a controller
suprnova make:controller users

# Create a model
suprnova make:model User

# Create a migration
suprnova make:migration create_posts_table

# Create a page component
suprnova make:page Home

# Generate TypeScript types
suprnova generate-types
```

## Environment Configuration

Configure your app using `.env`:

```env
# Server
PORT=8080

# Database
DATABASE_URL=sqlite:./database.db

# Inertia/Frontend
INERTIA_DEVELOPMENT=true
VITE_DEV_SERVER=http://localhost:5173
```

## Debugging

### Logging

suprnova uses Rust's standard logging. Enable debug output:

```bash
RUST_LOG=debug suprnova serve
```

### Database Queries

Enable SQL query logging:

```env
RUST_LOG=sea_orm=debug suprnova serve
```

## Testing

Run your test suite:

```bash
cargo test
```

### Frontend Tests

```bash
cd frontend
npm test
```

## Building for Production

### Build the Backend

```bash
cargo build --release
```

### Build the Frontend

```bash
cd frontend
npm run build
```

### Run Production Server

```bash
INERTIA_DEVELOPMENT=false ./target/release/your-app
```

## Useful Commands

| Command | Description |
|---------|-------------|
| `suprnova serve` | Start dev server |
| `suprnova serve --backend-only` | Start without Vite |
| `suprnova migrate` | Run migrations |
| `suprnova migrate:fresh` | Reset database |
| `suprnova db:sync` | Regenerate entities |
| `suprnova make:controller <name>` | Create controller |
| `suprnova make:model <name>` | Create model |
| `suprnova generate-types` | Generate TS types |
| `cargo build --release` | Production build |
