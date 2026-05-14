---
title: "Introduction"
description: "suprnova brings the DX of Laravel to Rust - type-safe, performant web apps with the velocity you love"
---

## What is suprnova?

suprnova is a Laravel-inspired web framework for Rust that combines:

- **Laravel's DX** - Familiar patterns like controllers, routes, middleware, and Eloquent-style models
- **Rust's Performance** - Native speed with zero-cost abstractions
- **Type Safety** - Catch errors at compile time, not runtime
- **Modern Frontend** - First-class Inertia.js + React integration

## Quick Start

Get started in under 5 minutes:

- [Quickstart Guide](/quickstart) — Create your first suprnova application step by step.

## Core Features

- [Routing](/core/routing) — Laravel-style routing with the `routes!` macro.
- [Controllers](/core/controllers) — Request handlers with dependency injection.
- [Database](/database/overview) — SeaORM with migrations and Model traits.
- [Inertia.js](/frontend/overview) — Build SPAs without writing an API.

## Tutorials

Learn by building:

- [Build a JSON API](/tutorials/json-api) — Create a complete REST API for todos.
- [Build with Inertia](/tutorials/inertia-crud) — Full-stack todo app with React.

## Why suprnova?

| Feature | Benefit |
|---------|---------|
| **Familiar Patterns** | If you know Laravel, you'll feel at home |
| **Compile-Time Safety** | Catch bugs before they reach production |
| **Native Performance** | Rust's speed without the complexity |
| **Full-Stack** | Backend + React frontend in one project |
| **Code Generation** | CLI tools to scaffold common patterns |

## Example

A simple controller in suprnova:

```rust
use suprnova::{Request, Response, json_response};

pub async fn index(_req: Request) -> Response {
    json_response!({
        "message": "Hello from suprnova!"
    })
}
```

With Inertia for a full-stack experience:

```rust
use suprnova::{Request, Response, inertia_response, InertiaProps};

#[derive(InertiaProps)]
pub struct HomeProps {
    pub title: String,
}

pub async fn index(_req: Request) -> Response {
    inertia_response!("Home", HomeProps {
        title: "Welcome to suprnova!".to_string(),
    })
}
```

## Get Started

- [Installation](/quickstart) — Install suprnova and create your first project.
