---
title: 'CLI Overview'
description: 'A powerful command-line tool for scaffolding and managing suprnova applications'
icon: 'terminal'
---

The suprnova CLI provides a comprehensive set of commands for creating projects, generating code, running development servers, and managing database migrations. Inspired by Laravel's Artisan CLI, it streamlines common development tasks.

## Installation

Install the Suprnova CLI:

```bash
cargo install --git https://github.com/entrepeneur4lyf/suprnova.git suprnova-cli
```

## Available Commands

### Project Commands

| Command | Description |
|---------|-------------|
| `suprnova new [name]` | Create a new Suprnova project |
| `suprnova serve` | Start development servers (backend + frontend) |

### Generator Commands

| Command | Description |
|---------|-------------|
| `suprnova make:controller <name>` | Generate a new controller |
| `suprnova make:action <name>` | Generate a new action |
| `suprnova make:middleware <name>` | Generate a new middleware |
| `suprnova make:error <name>` | Generate a new domain error |
| `suprnova make:migration <name>` | Generate a new database migration |
| `suprnova make:inertia <name>` | Generate a new Inertia.js page component |
| `suprnova generate-types` | Generate TypeScript types from Rust structs |

### Migration Commands

| Command | Description |
|---------|-------------|
| `suprnova migrate` | Run all pending migrations |
| `suprnova migrate:status` | Show migration status |
| `suprnova migrate:rollback` | Rollback the last migration(s) |
| `suprnova migrate:fresh` | Drop all tables and re-run migrations |
| `suprnova db:sync` | Sync database schema to entity files |

## Quick Start

```bash
# Create a new project
suprnova new my-app

# Navigate to project
cd my-app

# Start development server
suprnova serve

# Generate code
suprnova make:controller User
suprnova make:action CreateUser
suprnova make:middleware Auth

# Run migrations
suprnova migrate
```

## Getting Help

Use `--help` with any command to see available options:

```bash
suprnova --help
suprnova serve --help
suprnova make:controller --help
```
