---
title: 'Dependency Injection'
description: 'Manage dependencies with suprnovas Laravel-inspired service container'
icon: 'cube'
---

suprnova provides a powerful dependency injection (DI) container inspired by Laravel's service container. The container manages class dependencies, allows automatic resolution, and enables easy testing through dependency swapping.

## The App Container

The `App` struct is the central facade for dependency injection in suprnova. It provides static methods for registering and resolving services throughout your application.

```rust
use suprnova::App;

// Register a singleton
App::singleton(DatabaseConnection::new(&url));

// Resolve later anywhere in your app
let db = App::resolve::<DatabaseConnection>()?;
```

## Registering Services

### Singletons

Singletons are shared instances that persist for the application's lifetime. The same instance is returned every time you resolve the type:

```rust
use suprnova::App;

// Register a singleton
App::singleton(MyService::new());

// Or use the macro
singleton!(MyService::new());
```

### Factories

Factories create a new instance every time the service is resolved:

```rust
use suprnova::App;

// Register a factory
App::factory(|| RequestLogger::new());

// Or use the macro
factory!(|| RequestLogger::new());
```

### Trait Bindings

Bind a trait to a concrete implementation, enabling interface-based programming:

```rust
use std::sync::Arc;
use suprnova::App;

// Bind trait to implementation
App::bind::<dyn HttpClient>(Arc::new(RealHttpClient::new()));

// Or use the macro (auto-wraps in Arc)
bind!(dyn HttpClient, RealHttpClient::new());
```

### Factory Trait Bindings

Create a new implementation instance each time the trait is resolved:

```rust
use suprnova::App;
use std::sync::Arc;

// Bind trait to factory
App::bind_factory::<dyn HttpClient, _>(|| Arc::new(RealHttpClient::new()));

// Or use the macro
bind_factory!(dyn HttpClient, || RealHttpClient::new());
```

## Resolving Services

### Basic Resolution

Use `App::get()` for optional resolution or `App::resolve()` for required dependencies:

```rust
use suprnova::App;

// Optional - returns Option<T>
let service: Option<MyService> = App::get();

// Required - returns Result, enables ? operator
let service: MyService = App::resolve::<MyService>()?;
```

### Trait Resolution

Use `App::make()` or `App::resolve_make()` for trait objects:

```rust
use suprnova::App;
use std::sync::Arc;

// Optional - returns Option<Arc<dyn Trait>>
let client: Option<Arc<dyn HttpClient>> = App::make::<dyn HttpClient>();

// Required - returns Result, enables ? operator
let client: Arc<dyn HttpClient> = App::resolve_make::<dyn HttpClient>()?;
```

### Resolution in Controllers

The `?` operator makes dependency resolution clean and ergonomic in controllers:

```rust
use suprnova::{App, Request, Response, json_response};
use crate::actions::UserService;

pub async fn index(_req: Request) -> Response {
    // Resolve service - returns 500 error if not found
    let service = App::resolve::<UserService>()?;

    let users = service.list_all().await?;

    json_response!({
        "users": users
    })
}
```

## The `#[injectable]` Macro

The `#[injectable]` macro provides automatic dependency injection with zero boilerplate. It:

1. Automatically derives `Clone` (and `Default` for simple structs)
2. Registers the type as a singleton at application startup
3. Resolves `#[inject]` field dependencies automatically

### Simple Injectable

For structs without dependencies:

```rust
use suprnova::injectable;

#[injectable]
pub struct AppState {
    pub counter: u32,
}

// Automatically registered at startup
// Resolve via:
let state = App::resolve::<AppState>()?;
```

### Injectable with Dependencies

Use `#[inject]` to mark fields that should be resolved from the container:

```rust
use suprnova::injectable;

#[injectable]
pub struct UserService {
    #[inject]
    config: AppConfig,
    #[inject]
    logger: LoggerService,
}

impl UserService {
    pub fn process(&self) {
        // config and logger are automatically injected
        self.logger.info("Processing with config");
    }
}
```

Dependencies are resolved when the service is registered. Ensure dependencies are registered before dependents.

### Unit Structs

Unit structs are also supported:

```rust
use suprnova::injectable;

#[injectable]
pub struct StatelessService;

impl StatelessService {
    pub fn execute(&self) -> String {
        "Hello from StatelessService!".to_string()
    }
}
```

## Registration Methods

| Method | Description | Usage |
|--------|-------------|-------|
| `App::singleton(instance)` | Register shared instance | `App::singleton(MyService::new())` |
| `App::factory(closure)` | Register factory for new instances | `App::factory(\|\| MyService::new())` |
| `App::bind::<T>(arc)` | Bind trait to implementation | `App::bind::<dyn Trait>(Arc::new(impl))` |
| `App::bind_factory::<T>(closure)` | Bind trait to factory | `App::bind_factory::<dyn Trait>(\|\| Arc::new(impl))` |

## Resolution Methods

| Method | Returns | Error Handling |
|--------|---------|----------------|
| `App::get::<T>()` | `Option<T>` | Returns `None` if not found |
| `App::resolve::<T>()` | `Result<T, FrameworkError>` | Returns error if not found |
| `App::make::<dyn T>()` | `Option<Arc<T>>` | Returns `None` if not found |
| `App::resolve_make::<dyn T>()` | `Result<Arc<T>, FrameworkError>` | Returns error if not found |

## Checking Registration

Check if a service is registered before resolving:

```rust
use suprnova::App;

// Check concrete type
if App::has::<MyService>() {
    let service = App::get::<MyService>().unwrap();
}

// Check trait binding
if App::has_binding::<dyn HttpClient>() {
    let client = App::make::<dyn HttpClient>().unwrap();
}
```

## Convenience Macros

suprnova provides macros for cleaner registration syntax:

```rust
use suprnova::{singleton, factory, bind, bind_factory};

// Register concrete singleton
singleton!(DatabaseConnection::new(&url));

// Register concrete factory
factory!(|| RequestLogger::new());

// Bind trait to singleton (auto-wraps in Arc)
bind!(dyn HttpClient, RealHttpClient::new());

// Bind trait to factory (auto-wraps in Arc)
bind_factory!(dyn HttpClient, || RealHttpClient::new());
```

## Testing with the Container

suprnova provides `TestContainer` for isolated testing with fake implementations:

```rust
use suprnova::testing::{TestContainer, TestContainerGuard};
use suprnova::App;
use std::sync::Arc;

#[tokio::test]
async fn test_with_fake_service() {
    // Set up test container - automatically cleared when guard is dropped
    let _guard = TestContainer::fake();

    // Register fake implementations
    TestContainer::singleton(FakeDatabase::new());
    TestContainer::bind::<dyn HttpClient>(Arc::new(FakeHttpClient::new()));

    // App::resolve() will now return the fakes
    let db = App::resolve::<FakeDatabase>().unwrap();
    let client: Arc<dyn HttpClient> = App::make::<dyn HttpClient>().unwrap();

    // Test your code...
} // Container automatically cleared here
```

### TestContainer Methods

| Method | Description |
|--------|-------------|
| `TestContainer::fake()` | Create isolated test container, returns guard |
| `TestContainer::singleton(instance)` | Register fake singleton |
| `TestContainer::factory(closure)` | Register fake factory |
| `TestContainer::bind::<T>(arc)` | Bind fake trait implementation |
| `TestContainer::bind_factory::<T>(closure)` | Bind fake trait factory |

The `TestContainerGuard` ensures test isolation by automatically cleaning up when dropped.

## Manual Registration in Bootstrap

While `#[injectable]` provides automatic registration, you may need to manually register services that require runtime configuration (like database connections, external API clients, or services configured from environment variables).

The `bootstrap.rs` file is the central location for manual service registration:

```rust
// src/bootstrap.rs
use suprnova::{bind, global_middleware, singleton, App, DB};
use crate::middleware;

pub async fn register() {
    // Initialize database connection
    DB::init().await.expect("Failed to connect to database");

    // Global middleware (runs on every request in registration order)
    global_middleware!(middleware::LoggingMiddleware);

    // Register a trait binding with runtime config
    bind!(dyn CacheStore, RedisCache::new(&redis_url));

    // Register a concrete singleton with config
    singleton!(EmailService::new(&smtp_config));
}
```

The `bootstrap::register()` function is called from `main.rs` before the server starts:

```rust
// src/main.rs
#[tokio::main]
async fn main() {
    Config::init(std::path::Path::new("."));
    config::register_all();

    // Register services and global middleware
    bootstrap::register().await;

    let router = routes::register();
    Server::from_config(router).run().await.expect("Failed to start server");
}
```

> **Note:**
>
> For more details on the bootstrap file and when to use manual vs automatic registration, see the [Bootstrap documentation](/core/bootstrap).


## Auto-Registration

suprnova uses the `inventory` crate for compile-time service registration. Services marked with `#[injectable]` are automatically registered when `App::boot_services()` is called (this happens automatically in `Server::from_config()`).

```rust
use suprnova::injectable;

// This service is automatically registered at startup
#[injectable]
pub struct AutoRegisteredService {
    pub value: String,
}

// No manual registration needed!
// Just resolve:
let service = App::resolve::<AutoRegisteredService>()?;
```

## Practical Examples

### Service with Database Access

```rust
use suprnova::injectable;
use suprnova::database::{Model, ModelMut};
use crate::models::users;

#[injectable]
pub struct UserRepository;

impl UserRepository {
    pub async fn find_by_id(&self, id: i32) -> Option<users::Model> {
        users::Entity::find_by_id(id).await.ok()
    }

    pub async fn all(&self) -> Vec<users::Model> {
        users::Entity::all().await.unwrap_or_default()
    }
}
```

### Service with Injected Dependencies

```rust
use suprnova::injectable;

#[injectable]
pub struct NotificationService {
    #[inject]
    mailer: MailerService,
    #[inject]
    logger: LoggerService,
}

impl NotificationService {
    pub async fn send(&self, to: &str, message: &str) -> Result<(), Error> {
        self.logger.info(&format!("Sending notification to {}", to));
        self.mailer.send(to, "Notification", message).await
    }
}
```

### Using in Controller

```rust
use suprnova::{App, Request, Response, json_response};
use crate::actions::{UserRepository, NotificationService};

pub async fn notify_user(req: Request) -> Response {
    let user_id: i32 = req.param("id")?.parse().map_err(|_| {
        AppError::bad_request("Invalid user ID")
    })?;

    let repo = App::resolve::<UserRepository>()?;
    let notifications = App::resolve::<NotificationService>()?;

    let user = repo.find_by_id(user_id).await
        .ok_or_else(|| AppError::not_found("User not found"))?;

    notifications.send(&user.email, "Hello!").await?;

    json_response!({
        "success": true,
        "message": format!("Notified user {}", user.name)
    })
}
```

## Summary

| Feature | Usage |
|---------|-------|
| Register singleton | `App::singleton(instance)` or `singleton!(instance)` |
| Register factory | `App::factory(closure)` or `factory!(closure)` |
| Bind trait | `App::bind::<dyn T>(arc)` or `bind!(dyn T, impl)` |
| Resolve concrete | `App::resolve::<T>()?` |
| Resolve trait | `App::resolve_make::<dyn T>()?` |
| Auto-register | `#[injectable]` on struct |
| Inject dependency | `#[inject]` on field |
| Test faking | `TestContainer::fake()` |
