---
title: 'Code Generators'
description: 'Generate controllers, actions, middleware, errors, and more'
icon: 'wand-magic-sparkles'
---

suprnova provides generator commands to scaffold common application components with proper structure and boilerplate code.

## make:controller

Generate a new controller for handling HTTP requests.

```bash
suprnova make:controller <name>
```

### Examples

```bash
suprnova make:controller User
suprnova make:controller OrderItem
suprnova make:controller api/User
```

### Generated File

```rust
// src/controllers/user.rs
use suprnova::{handler, json_response, Request, Response};

#[handler]
pub async fn invoke(_req: Request) -> Response {
    json_response!({
        "controller": "User"
    })
}
```

### What It Does

1. Creates `src/controllers/<name>.rs` with `#[handler]` attribute
2. Updates `src/controllers/mod.rs` to export the controller

---

## make:action

Generate a new action for encapsulating business logic.

```bash
suprnova make:action <name>
```

### Examples

```bash
suprnova make:action CreateUser
suprnova make:action SendNotification
suprnova make:action ProcessPayment
```

### Generated File

```rust
// src/actions/create_user.rs
use suprnova::injectable;

#[injectable]
pub struct CreateUserAction {
    // Dependencies injected via container
}

impl CreateUserAction {
    pub fn execute(&self) {
        // TODO: Implement action logic
    }
}
```

### What It Does

1. Creates `src/actions/<name>.rs`
2. Updates `src/actions/mod.rs` to export the action
3. Action is automatically registered in the DI container

---

## make:middleware

Generate a new middleware for request/response processing.

```bash
suprnova make:middleware <name>
```

### Examples

```bash
suprnova make:middleware Auth
suprnova make:middleware RateLimit
suprnova make:middleware Cors
```

### Generated File

```rust
// src/middleware/auth.rs
use suprnova::middleware::{Middleware, Next};
use suprnova::{Request, Response};
use async_trait::async_trait;

pub struct AuthMiddleware;

#[async_trait]
impl Middleware for AuthMiddleware {
    async fn handle(&self, req: Request, next: Next) -> Response {
        // Before request handling

        let response = next.run(req).await;

        // After request handling

        response
    }
}
```

### What It Does

1. Creates `src/middleware/<name>.rs`
2. Updates `src/middleware/mod.rs` to export the middleware

---

## make:error

Generate a custom domain error with HTTP response conversion.

```bash
suprnova make:error <name>
```

### Examples

```bash
suprnova make:error UserNotFound
suprnova make:error PaymentFailed
suprnova make:error InsufficientStock
```

### Generated File

```rust
// src/errors/user_not_found.rs
use suprnova::domain_error;

#[domain_error(status = 500, message = "User not found")]
pub struct UserNotFound;
```

### What It Does

1. Creates `src/errors/<name>.rs`
2. Creates or updates `src/errors/mod.rs`
3. Generates a domain error with automatic HTTP response conversion

### Usage

```rust
use crate::errors::user_not_found::UserNotFound;

pub async fn show(req: Request) -> Response {
    let user = find_user(id).await
        .ok_or(UserNotFound)?;  // Returns 500 response

    json_response!({ "user": user })
}
```

---

## make:task

Generate a new scheduled task for background processing.

```bash
suprnova make:task <name>
```

### Examples

```bash
suprnova make:task CleanupLogs
suprnova make:task SendReminders
suprnova make:task BackupDatabase
```

### Generated File

```rust
// src/tasks/cleanup_logs_task.rs
use async_trait::async_trait;
use suprnova::{Task, TaskResult};

/// CleanupLogsTask - A scheduled task
///
/// Implement your task logic in the `handle()` method.
/// Register this task in `src/schedule.rs` with the fluent API.
pub struct CleanupLogsTask;

impl CleanupLogsTask {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Task for CleanupLogsTask {
    async fn handle(&self) -> TaskResult {
        // TODO: Implement your task logic here
        println!("Running CleanupLogsTask...");
        Ok(())
    }
}
```

### What It Does

1. Creates `src/tasks/<name>.rs` with `Task` trait implementation
2. Creates or updates `src/tasks/mod.rs` to export the task
3. Creates `src/schedule.rs` for registering tasks (if not exists)
4. Creates `src/bin/schedule.rs` scheduler binary (if not exists)

### Next Steps

After generating, register your task in `src/schedule.rs`:

```rust
use crate::tasks::cleanup_logs_task;

pub fn register(schedule: &mut Schedule) {
    schedule.add(
        schedule.task(cleanup_logs_task::CleanupLogsTask::new())
            .daily()
            .at("03:00")
            .name("cleanup_logs_task")
            .description("TODO: Add task description")
    );
}
```

Then run the scheduler:

```bash
suprnova schedule:work  # Daemon mode
suprnova schedule:run   # Run once
```

---

## make:inertia

Generate an Inertia.js page component.

```bash
suprnova make:inertia <name>
```

### Examples

```bash
suprnova make:inertia About
suprnova make:inertia UserProfile
suprnova make:inertia Dashboard
```

### Generated Files

Creates a React component in `frontend/src/pages/`:

```tsx
// frontend/src/pages/About.tsx
export default function About() {
    return (
        <div>
            <h1>About</h1>
        </div>
    );
}
```

---

## generate-types

Generate TypeScript types from Rust `InertiaProps` structs.

```bash
suprnova generate-types [options]
```

### Options

| Option | Description |
|--------|-------------|
| `-o, --output <PATH>` | Output file path (default: `frontend/src/types/inertia-props.ts`) |
| `-w, --watch` | Watch for changes and regenerate |

### Examples

```bash
# Generate types once
suprnova generate-types

# Watch mode
suprnova generate-types --watch

# Custom output path
suprnova generate-types --output frontend/src/types/props.ts
```

### How It Works

Scans your Rust code for structs implementing `InertiaProps` and generates TypeScript interfaces:

```rust
// Rust
#[derive(InertiaProps)]
pub struct UserPageProps {
    pub user: User,
    pub posts: Vec<Post>,
}
```

```typescript
// Generated TypeScript
export interface UserPageProps {
    user: User;
    posts: Post[];
}
```

---

## Summary

| Command | Creates | Location |
|---------|---------|----------|
| `make:controller <name>` | Controller | `src/controllers/` |
| `make:action <name>` | Action | `src/actions/` |
| `make:middleware <name>` | Middleware | `src/middleware/` |
| `make:error <name>` | Domain Error | `src/errors/` |
| `make:task <name>` | Scheduled Task | `src/tasks/` |
| `make:inertia <name>` | Page Component | `frontend/src/pages/` |
| `make:migration <name>` | Migration | `migrations/` |
| `generate-types` | TypeScript Types | `frontend/src/types/` |
