# Testing

suprnova provides testing utilities that make it easy to write tests for your actions, services, and controllers with full database support using in-memory SQLsuprnova. Suprnova also offers Jest-like testing macros for better test organization and clearer failure output.

## Quick Start

The simplest way to write a test with database support using Jest-like syntax:

```rust
use suprnova::{describe, test, expect};
use suprnova::testing::TestDatabase;

describe!("UserService", {
    test!("creates a user successfully", async fn(db: TestDatabase) {
        let action = App::resolve::<CreateUserAction>().unwrap();
        let user = action.execute("test@example.com").await.unwrap();

        expect!(user.id).to_be_greater_than(0);
        expect!(user.email).to_equal("test@example.com".to_string());
    });
});
```

Or using the traditional attribute macro:

```rust
use suprnovarnsuprnova::suprnova_test;
use suprnova::testing::TestDatabase;

#[suprnova_test]
async fn test_user_creation(db: TestDatabase) {
    let action = CreateUserAction::new();
    let user = action.execute("test@example.com").await.unwrap();

    assert!(user.id > 0);
}
```

## Jest-like Testing (Recommended)

suprnova provides Jest-like macros for better test organization and clearer assertion output.

### The `describe!` Macro

Group related tests with descriptive names:

```rust
use suprnova::{describe, test, expect};
use suprnova::testing::TestDatabase;

describe!("ListTodosAction", {
    test!("returns empty list when no todos exist", async fn(db: TestDatabase) {
        let action = App::resolve::<ListTodosAction>().unwrap();
        let todos = action.execute().await.unwrap();

        expect!(todos).to_be_empty();
    });

    test!("returns all todos", async fn(db: TestDatabase) {
        // Create test data
        Todo::create().title("Test Todo".to_string()).save().await.unwrap();

        let action = App::resolve::<ListTodosAction>().unwrap();
        let todos = action.execute().await.unwrap();

        expect!(todos).to_have_length(1);
    });

    // Nested describe blocks for sub-groups
    describe!("with pagination", {
        test!("returns first page", async fn(db: TestDatabase) {
            // ...
        });
    });
});
```

### The `test!` Macro

Define individual test cases with three syntax options:

```rust
// Async test with database
test!("creates a user", async fn(db: TestDatabase) {
    let action = App::resolve::<CreateUserAction>().unwrap();
    let user = action.execute("test@example.com").await.unwrap();
    expect!(user.email).to_equal("test@example.com".to_string());
});

// Async test without database
test!("calculates sum", async fn() {
    let result = calculate(1, 2).await;
    expect!(result).to_equal(3);
});

// Sync test
test!("adds numbers", fn() {
    expect!(1 + 1).to_equal(2);
});
```

### The `expect!` Macro

Fluent assertions with clear failure output:

```rust
// Equality
expect!(actual).to_equal(expected);
expect!(actual).to_not_equal(unexpected);

// Boolean
expect!(condition).to_be_true();
expect!(condition).to_be_false();

// Option
expect!(option).to_be_some();
expect!(option).to_be_none();

// Result
expect!(result).to_be_ok();
expect!(result).to_be_err();

// Strings
expect!(string).to_contain("substring");
expect!(string).to_start_with("prefix");
expect!(string).to_end_with("suffix");
expect!(string).to_have_length(10);
expect!(string).to_be_empty();

// Collections
expect!(vec).to_have_length(3);
expect!(vec).to_contain(&item);
expect!(vec).to_be_empty();

// Numeric comparisons
expect!(10).to_be_greater_than(5);
expect!(5).to_be_less_than(10);
expect!(10).to_be_greater_than_or_equal(10);
expect!(5).to_be_less_than_or_equal(5);
```

### Clear Failure Output

When an assertion fails, you get clear output with the test name:

```text
Test: "creates a user"
  at src/actions/user_action.rs:25

  expect!(actual).to_equal(expected)

  Expected: "test@example.com"
  Received: "wrong@email.com"
```

## Testing Approaches (Traditional)

suprnova also provides traditional ways to write database-enabled tests:

### 1. Attribute Macro (Recommended)

The `#[suprnova_test]` attribute macro is the cleanest way to write tests:

```rust
use suprnovarnsuprnova::suprnova_test;
use suprnova::testing::TestDatabase;

#[suprnova_test]
async fn test_create_todo(db: TestDatabase) {
    // db is an in-memory SQLite database with all migrations applied
    let action = CreateTodoAction::new();
    let todo = action.execute("Buy groceries").await.unwrap();

    // Query directly using db.conn()
    let found = todos::Entity::find_by_id(todo.id)
        .one(db.conn())
        .await
        .unwrap();

    assert!(found.is_some());
    assert_eq!(found.unwrap().title, "Buy groceries");
}
```

### 2. Helper Macro

For more control, use the `test_database!` macro:

```rust
use suprnova::test_database;

#[tokio::test]
async fn test_todo_list() {
    let db = test_database!();

    // Create some test data
    let action = CreateTodoAction::new();
    action.execute("Task 1").await.unwrap();
    action.execute("Task 2").await.unwrap();

    // Test the list action
    let list_action = ListTodosAction::new();
    let todos = list_action.execute().await.unwrap();

    assert_eq!(todos.len(), 2);
}
```

## How It Works

When you use `#[suprnova_test]`:

1. **Services Bootstrapped**: All services marked with `#[injectable]` are automatically registered, so `App::resolve::<T>()` works just like in production
2. **Fresh Database**: A new in-memory SQLite database is created for each test
3. **Migrations Applied**: Your `crate::migrations::Migrator` runs automatically
4. **Automatic Integration**: The test database is registered in the DI container, so any code using `DB::connection()` or `#[inject] db: Database` automatically uses the test database
5. **Complete Isolation**: Each test is fully isolated - no data leaks between tests

> **Note:**
>
> The `#[suprnova_test]` macro calls `App::init()` and `App::boot_services()` before your test runs, ensuring all injectable services are available.


## Testing Actions

Actions marked with `#[injectable]` can be resolved from the container in tests:

```rust
// Your action
#[injectable]
pub struct CreateUserAction {
    #[inject]
    db: Database,
}

impl CreateUserAction {
    pub async fn execute(&self, email: &str) -> Result<users::Model, FrameworkError> {
        let user = users::ActiveModel {
            email: Set(email.to_string()),
            ..Default::default()
        };
        users::Entity::insert_one(user).await
    }
}

// Your test - resolve the action from the container
#[suprnova_test]
async fn test_create_user(db: TestDatabase) {
    // Resolve the action from the DI container
    let action = App::resolve::<CreateUserAction>().unwrap();
    let user = action.execute("test@example.com").await.unwrap();

    // Verify in database
    let count = users::Entity::find()
        .count(db.conn())
        .await
        .unwrap();
    assert_eq!(count, 1);
}
```

## Custom Migrator

By default, both macros use `crate::migrations::Migrator`. If your migrator is in a different location:

```rust
// With attribute macro
#[suprnova_test(migrator = my_crate::CustomMigrator)]
async fn test_with_custom_migrator(db: TestDatabase) {
    // ...
}

// With helper macro
#[tokio::test]
async fn test_with_custom_migrator() {
    let db = test_database!(my_crate::CustomMigrator);
    // ...
}
```

## Direct Database Access

The `TestDatabase` struct provides methods for direct database queries:

```rust
#[suprnova_test]
async fn test_database_queries(db: TestDatabase) {
    // Use db.conn() for SeaORM queries
    let users = users::Entity::find()
        .all(db.conn())
        .await
        .unwrap();

    // Or use db.db() to get the DbConnection
    let conn = db.db();
}
```

## Test Without Database Parameter

If you don't need direct database access in your test but still want the database set up:

```rust
#[suprnova_test]
async fn test_action_indirectly() {
    // Database is set up, but we don't need direct access
    // Actions using DB::connection() still work
    let action = MyAction::new();
    let result = action.execute().await.unwrap();
    assert!(result.success);
}
```

## Best Practices

### 1. One Assertion Per Test

Keep tests focused on a single behavior:

```rust
#[suprnova_test]
async fn test_user_email_is_stored(db: TestDatabase) {
    let action = CreateUserAction::new();
    let user = action.execute("test@example.com").await.unwrap();

    assert_eq!(user.email, "test@example.com");
}

#[suprnova_test]
async fn test_user_gets_default_role(db: TestDatabase) {
    let action = CreateUserAction::new();
    let user = action.execute("test@example.com").await.unwrap();

    assert_eq!(user.role, "user");
}
```

### 2. Test Edge Cases

```rust
#[suprnova_test]
async fn test_create_user_with_duplicate_email(db: TestDatabase) {
    let action = CreateUserAction::new();

    // First user succeeds
    action.execute("test@example.com").await.unwrap();

    // Second user with same email should fail
    let result = action.execute("test@example.com").await;
    assert!(result.is_err());
}
```

### 3. Use Factories for Test Data

Create helper functions to generate test data:

```rust
async fn create_test_user(db: &TestDatabase, email: &str) -> users::Model {
    let user = users::ActiveModel {
        email: Set(email.to_string()),
        ..Default::default()
    };
    user.insert(db.conn()).await.unwrap()
}

#[suprnova_test]
async fn test_delete_user(db: TestDatabase) {
    let user = create_test_user(&db, "test@example.com").await;

    let action = DeleteUserAction::new();
    action.execute(user.id).await.unwrap();

    let found = users::Entity::find_by_id(user.id)
        .one(db.conn())
        .await
        .unwrap();
    assert!(found.is_none());
}
```

## Running Tests

Run your tests using cargo:

```bash
# Run all tests
cargo test

# Run a specific test
cargo test test_user_creation

# Run tests with output
cargo test -- --nocapture
```

## The `testing` feature and production builds

`suprnova` exposes test helpers (`Storage::fake()`, `TestContainer`,
`TestDatabase`, crypto rotation hooks like `_test_install_keyring`) behind
a Cargo feature named `testing`. The feature is part of the default
feature set so consuming test suites pick them up for free with:

```toml
[dependencies]
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }

[dev-dependencies]
# `testing` is on transitively via the dependency above — nothing extra.
```

The test hooks themselves are `#[doc(hidden)]` and prefixed with `_test_`,
so they aren't reachable from idiomatic application code even when the
feature is on. The load-bearing safeguard is `Server::from_config`: it
validates `APP_KEY` on **every** boot, not only when the key ring is
uninitialized. A pre-installed test key cannot bypass that check —
boot fails fast if `APP_KEY` is missing or malformed regardless of
whether anything in-process pre-installed a key.

If you'd rather the helpers not be present in your production artifact
at all (defense in depth), depend on `suprnova` with default features
off and enable only what you ship:

```toml
[dependencies]
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git", default-features = false, features = ["..."] }

[dev-dependencies]
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git", features = ["testing", "..."] }
```

This is a tightening, not a fix — boot validation closes the actual
exploit regardless of which posture you pick.
