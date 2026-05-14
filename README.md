# suprnova

**A Laravel-inspired web framework for Rust**

[![Crates.io](https://img.shields.io/crates/v/suprnova.svg)](https://crates.io/crates/suprnova)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Build web applications in Rust with the developer experience you love from Laravel and Rails. suprnova gives you expressive routing, powerful tooling, and batteries-included features—without sacrificing Rust's performance.

[Website](https://suprnova.dev/) | [Documentation](https:suprnovauprnova.dev/)

## Quick Start

```bash
cargo install suprnova-cli
suprnova new myapp
cd myapp
suprnova serve
```

Your app is now running at `http://localhost:8000`

## Example

If you've used Laravel or Rails, this will feel familiar:

```rust
use suprnova::{get, post, routes, json_response, Request, Response};

routes! {
    get("/", index),
    get("/users/{id}", show),
    post("/users", store),
}

async fn index(_req: Request) -> Response {
    json_response!({ "message": "Welcome to suprnova!" })
}

async fn show(req: Request) -> Response {
    let id = req.param("id")?;
    json_response!({ "user": { "id": id } })
}

async fn store(_req: Request) -> Response {
    // Your logic here
    json_response!({ "created": true })
}
```

## Why suprnova?

- **Familiar patterns** — Routes, controllers, middleware, service container
- **CLI generators** — `suprnova make:controllesuprnova `suprnova makesuprnovadel`, `suprnova migrate`
- **Database built-in** — Migrations, ORM, query builder
- **Modern frontend** — First-class Inertia.js + React with automatic TypeScript types
- **Rust performance** — All the safety and speed, none of the ceremony

## Durable Workflows

suprnova includes a Postgres-backed workflow engine with durable steps and retries.

```rust
use suprnova::{workflow, workflow_step, start_workflow, FrameworkError};

#[workflow_step]
async fn fetch_user(user_id: i64) -> Result<String, FrameworkError> {
    Ok(format!("user:{}", user_id))
}

#[workflow_step]
async fn send_welcome_email(user: String) -> Result<(), FrameworkError> {
    println!("Sending email to {}", user);
    Ok(())
}

#[workflow]
async fn welcome_flow(user_id: i64) -> Result<(), FrameworkError> {
    let user = fetch_user(user_id).await?;
    send_welcome_email(user).await?;
    Ok(())
}

// Enqueue
// let handle = start_workflow!(welcome_flow, 123).await?;
```

Run the worker in production:

```bash
suprnova workflow:work
```

## End-to-End Type Safety

suprnova provides automatic TypeScript type generation from your Rust structs. Define your props once in Rust, and use them with full type safety in React.

**Define props in Rust:**

```rust
use suprnova::{InertiaProps, inertia_response, Request, Response};

#[derive(InertiaProps)]
pub struct User {
    pub name: String,
    pub email: String,
}

#[derive(InertiaProps)]
pub struct HomeProps {
    pub title: String,
    pub user: User,
}

pub async fn index(_req: Request) -> Response {
    inertia_response!("Home", HomeProps {
        title: "Welcome!".to_string(),
        user: User {
            name: "John".to_string(),
            email: "john@example.com".to_string(),
        },
    })
}
```

**Run type generation:**

```bash
suprnova generate-types
```

**TypeScript types are auto-generated:**

```typescript
// frontend/src/types/inertia-props.ts (auto-generated)
export interface HomeProps {
  title: string;
  user: User;
}

export interface User {
  name: string;
  email: string;
}
```

**Use in your React components with full autocomplete:**

```tsx
import { HomeProps } from "../types/inertia-props";

export default function Home({ title, user }: HomeProps) {
  return (
    <div>
      <h1>{title}</h1>
      <p>Welcome, {user.name}!</p>
      <p>Email: {user.email}</p>
    </div>
  );
}
```

Change a field in Rust, regenerate types, and TypeScript will catch any mismatches at compile time.

## Documentation

Ready to build something? Check out the [full documentation](https://suprnova.dev/) to get started.

## License

MIT
