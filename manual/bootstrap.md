# Bootstrap

The `bootstrap.rs` file is your application's central configuration point for registering services, global middleware, and performing runtime initialization. It's called during application startup, before the server begins handling requests.

## Overview

Every suprnova application has a `src/bootstrap.rs` file that contains a `register()` function:

```rust
// src/bootstrap.rs
use suprnova::{bind, global_middleware, singleton, App, DB};
use crate::middleware;

pub async fn register() {
    // Initialize database connection
    DB::init().await.expect("Failed to connect to database");

    // Global middleware (runs on every request)
    global_middleware!(middleware::LoggingMiddleware);

    // Register services that need runtime configuration
    // bind!(dyn CacheStore, RedisCache::new(&redis_url));
}
```

## When Bootstrap Runs

The bootstrap file is called in `main.rs` during the application startup sequence:

```rust
// src/main.rs
#[tokio::main]
async fn main() {
    // 1. Initialize framework configuration (loads .env files)
    Config::init(std::path::Path::new("."));

    // 2. Register application configs
    config::register_all();

    // 3. Register services and global middleware
    bootstrap::register().await;

    // 4. Register routes
    let router = routes::register();

    // 5. Start server
    Server::from_config(router)
        .run()
        .await
        .expect("Failed to start server");
}
```

This sequence ensures that environment variables and configuration are available before services are initialized.

## What to Register in Bootstrap

### Database Initialization

Initialize the database connection to make it available throughout your application:

```rust
use suprnova::DB;

pub async fn register() {
    DB::init().await.expect("Failed to connect to database");
}
```

### Global Middleware

Register middleware that should run on every request:

```rust
use suprnova::global_middleware;
use crate::middleware;

pub async fn register() {
    // Middleware runs in registration order
    global_middleware!(middleware::LoggingMiddleware);
    global_middleware!(middleware::CorsMiddleware);
    global_middleware!(middleware::SecurityHeadersMiddleware);
}
```

> **Note:**
>
> Global middleware runs on every request in the order registered. For route-specific middleware, see the [Middleware documentation](middleware.md).


### Services with Runtime Configuration

Register services that need environment variables, config files, or other runtime values:

```rust
use suprnova::{singleton, bind};

pub async fn register() {
    // Services configured from environment
    let redis_url = std::env::var("REDIS_URL").unwrap_or_default();
    bind!(dyn CacheStore, RedisCache::new(&redis_url));

    // Services with complex initialization
    let smtp_config = SmtpConfig::from_env();
    singleton!(EmailService::new(smtp_config));

    // External API clients
    let api_key = std::env::var("STRIPE_API_KEY").expect("STRIPE_API_KEY required");
    singleton!(StripeClient::new(&api_key));
}
```

### Trait Bindings

Bind interfaces to concrete implementations for dependency injection:

```rust
use suprnova::bind;
use std::sync::Arc;

pub async fn register() {
    // Bind trait to implementation
    bind!(dyn PaymentGateway, StripeGateway::new());
    bind!(dyn EmailSender, SmtpEmailSender::new());
    bind!(dyn CacheStore, RedisCache::new());
}
```

## Bootstrap vs `#[injectable]`

suprnova provides two ways to register services:

| Feature | `#[injectable]` Macro | Bootstrap Registration |
|---------|----------------------|------------------------|
| **When to use** | Simple services with no runtime config | Services needing env vars, config, or complex setup |
| **Registration** | Automatic at compile-time | Manual in `bootstrap.rs` |
| **Dependencies** | Via `#[inject]` attribute | Passed to constructor |
| **Flexibility** | Limited to defaults | Full control over initialization |

### Use `#[injectable]` for:

```rust
// Simple services without runtime configuration
#[injectable]
pub struct UserService;

// Services with injected dependencies
#[injectable]
pub struct OrderService {
    #[inject]
    user_service: UserService,
}
```

### Use Bootstrap for:

```rust
pub async fn register() {
    // Database connections
    DB::init().await.expect("Database connection failed");

    // Services configured from environment
    let api_key = std::env::var("API_KEY")?;
    singleton!(ExternalApiClient::new(&api_key));

    // Conditional service registration
    if cfg!(debug_assertions) {
        singleton!(MockPaymentGateway::new());
    } else {
        singleton!(RealPaymentGateway::new());
    }
}
```

## Complete Bootstrap Example

Here's a comprehensive example showing common patterns:

```rust
//! Application Bootstrap
//!
//! Register global middleware and services that need runtime configuration.

use suprnova::{bind, global_middleware, singleton, App, DB};
use crate::middleware;
use std::sync::Arc;

pub async fn register() {
    // ═══════════════════════════════════════════════════════
    // Database
    // ═══════════════════════════════════════════════════════
    DB::init().await.expect("Failed to connect to database");

    // ═══════════════════════════════════════════════════════
    // Global Middleware (runs on every request, in order)
    // ═══════════════════════════════════════════════════════
    global_middleware!(middleware::LoggingMiddleware);
    global_middleware!(middleware::CorsMiddleware);

    // ═══════════════════════════════════════════════════════
    // Cache
    // ═══════════════════════════════════════════════════════
    let redis_url = std::env::var("REDIS_URL")
        .unwrap_or_else(|_| "redis://localhost:6379".to_string());
    bind!(dyn CacheStore, RedisCache::new(&redis_url));

    // ═══════════════════════════════════════════════════════
    // Email
    // ═══════════════════════════════════════════════════════
    let smtp_host = std::env::var("SMTP_HOST").unwrap_or_default();
    let smtp_port = std::env::var("SMTP_PORT")
        .unwrap_or_else(|_| "587".to_string())
        .parse()
        .unwrap_or(587);
    singleton!(EmailService::new(&smtp_host, smtp_port));

    // ═══════════════════════════════════════════════════════
    // External Services
    // ═══════════════════════════════════════════════════════
    if let Ok(stripe_key) = std::env::var("STRIPE_SECRET_KEY") {
        singleton!(StripeClient::new(&stripe_key));
    }
}
```

## Summary

| Task | Method |
|------|--------|
| Initialize database | `DB::init().await` |
| Register global middleware | `global_middleware!(Middleware)` |
| Register singleton | `singleton!(Service::new())` |
| Register factory | `factory!(\|\| Service::new())` |
| Bind trait to impl | `bind!(dyn Trait, Implementation::new())` |
| File location | `src/bootstrap.rs` |
