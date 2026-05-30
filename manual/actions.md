---
title: 'Actions'
description: 'Encapsulate business logic with injectable action classes'
icon: 'bolt'
---

Actions in suprnova are injectable service classes that encapsulate your application's business logic. Inspired by Laravel's single-action classes, actions promote clean code organization by separating business logic from controllers. The `#[injectable]` macro provides automatic dependency injection and singleton registration.

## Generating Actions

The fastest way to create a new action is using the suprnova CLI:

```bash
suprnova make:action CreateUser
```

This command will:
1. Create `src/actions/create_user.rs` with an action stub
2. Update `src/actions/mod.rs` to export the new action

```bash Examples
# Creates create_user.rs in src/actions/
suprnova make:action CreateUser

# Creates send_notification.rs in src/actions/
suprnova make:action SendNotification

# Action name is converted to snake_case for the file
suprnova make:action ProcessPayment  # Creates process_payment.rs
```

```rust Generated File
//! CreateUser action

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

## Action Structure

Actions are structs marked with `#[injectable]` that contain business logic:

```rust
use suprnova::injectable;

#[injectable]
pub struct ExampleAction {
    // Optional: injected dependencies
}

impl ExampleAction {
    pub fn execute(&self) -> String {
        "Hello from ExampleAction!".to_string()
    }
}
```

### The `#[injectable]` Macro

The `#[injectable]` macro provides powerful dependency injection:

- **Automatic registration**: Actions are automatically registered as singletons in the container
- **Zero boilerplate**: No manual container configuration required
- **Compile-time safety**: Type-safe dependency resolution

## Using Actions in Controllers

Resolve actions from the container using `App::resolve()`:

```rust
use suprnova::{App, Request, Response, json_response};
use crate::actions::example_action::ExampleAction;

pub async fn index(_req: Request) -> Response {
    // Resolve the action from the container
    let action = App::resolve::<ExampleAction>()?;

    // Execute the action
    let message = action.execute();

    json_response!({
        "message": message
    })
}
```

The `?` operator handles the case where the action isn't registered, returning an appropriate error response.

## Async Actions

Actions can be async for database operations and other I/O tasks:

```rust
use suprnova::injectable;
use suprnova::database::{Model, ModelMut};
use suprnova::error::FrameworkError;
use sea_orm::Set;
use crate::models::todos;

#[injectable]
pub struct CreateTodoAction;

impl CreateTodoAction {
    pub async fn execute(&self, title: String) -> Result<todos::Model, FrameworkError> {
        let new_todo = todos::ActiveModel {
            title: Set(title),
            description: Set(None),
            ..Default::default()
        };

        todos::Entity::insert_one(new_todo).await
    }
}

#[injectable]
pub struct ListTodosAction;

impl ListTodosAction {
    pub async fn execute(&self) -> Result<Vec<todos::Model>, FrameworkError> {
        todos::Entity::all().await
    }
}
```

Using async actions in controllers:

```rust
use suprnova::{App, Request, Response, json_response};
use crate::actions::todo_action::{CreateTodoAction, ListTodosAction};

pub async fn index(_req: Request) -> Response {
    let action = App::resolve::<ListTodosAction>()?;
    let todos = action.execute().await?;

    json_response!({
        "todos": todos
    })
}

pub async fn store(_req: Request) -> Response {
    let action = App::resolve::<CreateTodoAction>()?;
    let todo = action.execute("New Todo".to_string()).await?;

    json_response!({
        "todo": todo
    })
}
```

## Actions with Dependencies

Actions can have dependencies injected via the `#[inject]` attribute:

```rust
use suprnova::injectable;

#[injectable]
pub struct SendEmailAction {
    #[inject]
    mailer: MailerService,
    #[inject]
    logger: LoggerService,
}

impl SendEmailAction {
    pub async fn execute(&self, to: &str, subject: &str, body: &str) -> Result<(), Error> {
        self.logger.info(&format!("Sending email to {}", to));
        self.mailer.send(to, subject, body).await
    }
}
```

Dependencies are automatically resolved from the container when the action is resolved.

## When to Use Actions

Actions are ideal for:

| Use Case | Example |
|----------|---------|
| Business operations | `CreateOrderAction`, `ProcessPaymentAction` |
| Data transformations | `CalculateTotalsAction`, `GenerateReportAction` |
| External integrations | `SendEmailAction`, `SyncInventoryAction` |
| Complex queries | `SearchProductsAction`, `GetDashboardStatsAction` |
| Multi-step processes | `RegisterUserAction`, `CheckoutAction` |

### Actions vs Controllers

| Controllers | Actions |
|-------------|---------|
| Handle HTTP requests | Contain business logic |
| Route-specific | Reusable across routes |
| Thin and focused | Rich domain logic |
| Call actions | Called by controllers |

## File Organization

The standard file structure for actions:

```
src/
├── actions/
│   ├── mod.rs              # Re-export all actions
│   ├── example_action.rs   # Example action
│   ├── todo_action.rs      # Todo-related actions
│   ├── user/               # Grouped user actions
│   │   ├── mod.rs
│   │   ├── create_user.rs
│   │   └── update_user.rs
│   └── order/              # Grouped order actions
│       ├── mod.rs
│       ├── create_order.rs
│       └── process_payment.rs
├── controllers/
└── main.rs
```

**src/actions/mod.rs:**
```rust
pub mod example_action;
pub mod todo_action;
pub mod user;
pub mod order;
```

## Practical Examples

### User Registration Action

```rust
use suprnova::injectable;
use suprnova::error::FrameworkError;
use sea_orm::Set;
use crate::models::users;

#[injectable]
pub struct RegisterUserAction;

impl RegisterUserAction {
    pub async fn execute(
        &self,
        email: String,
        password: String,
        name: String,
    ) -> Result<users::Model, FrameworkError> {
        // Hash password (simplified)
        let hashed_password = hash_password(&password);

        let new_user = users::ActiveModel {
            email: Set(email),
            password: Set(hashed_password),
            name: Set(name),
            ..Default::default()
        };

        users::Entity::insert_one(new_user).await
    }
}

fn hash_password(password: &str) -> String {
    // Password hashing logic
    format!("hashed_{}", password)
}
```

### Action with Return Types

```rust
use suprnova::injectable;

pub struct DashboardStats {
    pub total_users: i64,
    pub total_orders: i64,
    pub revenue: f64,
}

#[injectable]
pub struct GetDashboardStatsAction;

impl GetDashboardStatsAction {
    pub async fn execute(&self) -> Result<DashboardStats, FrameworkError> {
        // Fetch statistics from database
        let total_users = users::Entity::count().await?;
        let total_orders = orders::Entity::count().await?;
        let revenue = orders::Entity::sum_revenue().await?;

        Ok(DashboardStats {
            total_users,
            total_orders,
            revenue,
        })
    }
}
```

### Using Multiple Actions in a Controller

```rust
use suprnova::{App, Request, Response, json_response};
use crate::actions::{
    user::GetUserAction,
    order::GetUserOrdersAction,
    notification::MarkNotificationsReadAction,
};

pub async fn dashboard(req: Request) -> Response {
    let user_id = req.param("id")?;

    // Resolve and execute multiple actions
    let get_user = App::resolve::<GetUserAction>()?;
    let get_orders = App::resolve::<GetUserOrdersAction>()?;
    let mark_read = App::resolve::<MarkNotificationsReadAction>()?;

    let user = get_user.execute(user_id).await?;
    let orders = get_orders.execute(user_id).await?;
    mark_read.execute(user_id).await?;

    json_response!({
        "user": user,
        "orders": orders
    })
}
```

## Summary

| Feature | Usage |
|---------|-------|
| Generate action | `suprnova make:action Name` |
| Make injectable | `#[injectable]` on struct |
| Inject dependency | `#[inject]` on field |
| Resolve action | `App::resolve::<ActionType>()?` |
| Sync execute | `action.execute()` |
| Async execute | `action.execute().await?` |
| File location | `src/actions/` |
