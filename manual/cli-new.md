---
title: 'suprnova new'
description: 'Create a new suprnova project'
icon: 'folder-plus'
---

The `suprnova new` command creates asuprnovaw Suprnova project with all the necessary files and directory structure.

## Usage

```bash
suprnova new [name] [options]
```

## Arguments

| Argument | Description |
|----------|-------------|
| `name` | The name of the project (optional, will prompt if not provided) |

## Options

| Option | Description |
|--------|-------------|
| `--no-interaction` | Skip all prompts and use defaults |
| `--no-git` | Skip git initialization |

## Examples

### Interactive Mode

```bash
suprnova new
```

This will prompt you for:
- Project name
- Database type (SQLite, PostgreSQL, MySQL)
- Whether to include Inertia.js for frontend

### Quick Start

```bash
suprnova new my-app
```

Creates a new project named "my-app" with interactive prompts for options.

### Non-Interactive Mode

```bash
suprnova new my-app --no-interaction
```

Creates a project with all default settings:
- SQLite database
- Inertia.js frontend with React
- Git repository initialized

### Without Git

```bash
suprnova new my-app --no-git
```

Creates the project without initializing a git repository.

## Generated Structure

```
my-app/
├── src/
│   ├── actions/           # Business logic actions
│   ├── controllers/       # HTTP controllers
│   ├── middleware/        # Custom middleware
│   ├── models/            # Database models
│   ├── config/            # Application configuration
│   ├── bootstrap.rs       # Service registration
│   ├── routes.rs          # Route definitions
│   └── main.rs            # Application entry point
├── frontend/              # Inertia.js frontend (if selected)
│   ├── src/
│   │   ├── pages/         # React page components
│   │   └── types/         # TypeScript types
│   ├── package.json
│   └── vite.config.ts
├── migrations/            # Database migrations
├── .env                   # Environment variables
├── .env.example           # Environment template
├── Cargo.toml             # Rust dependencies
└── README.md
```

## What's Included

- Pre-configured Axum web server
- SeaORM database integration
- Inertia.js with React (optional)
- TypeScript type generation
- Hot-reload development server
- Example controller, action, and middleware
- Database migration setup
