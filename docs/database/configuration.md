---
title: 'Database Configuration'
description: 'Configure database connections and connection pooling'
icon: 'gear'
---

suprnova provides flexible database configuration through environment variables and a programmatic API.

## Environment Variables

The simplest way to configure your database is through the `.env` file:

```env
# .env
DATABASE_URL=sqlite:./database.db
```

### Connection String Formats

| Database | Format | Example |
|----------|--------|---------|
| SQLite | `sqlite:<path>` | `sqlite:./database.db` |
| PostgreSQL | `postgres://user:pass@host:port/db` | `postgres://admin:secret@localhost:5432/myapp` |
| MySQL | `mysql://user:pass@host:port/db` | `mysql://root:password@localhost:3306/myapp` |

## DatabaseConfig

For programmatic configuration, use `DatabaseConfig`:

```rust
use suprnova::database::DatabaseConfig;

let config = DatabaseConfig::from_env();
```

### Builder Pattern

You can also build configuration manually:

```rust
use suprnova::database::DatabaseConfig;

let config = DatabaseConfig::builder()
    .url("postgres://localhost/myapp")
    .max_connections(10)
    .min_connections(2)
    .connect_timeout_seconds(30)
    .idle_timeout_seconds(600)
    .build();
```

## Connection Pooling

suprnova automatically manages a connection pool for optimal performance. Configure pooling settings through `DatabaseConfig`:

| Setting | Default | Description |
|---------|---------|-------------|
| `max_connections` | `10` | Maximum number of connections in the pool |
| `min_connections` | `1` | Minimum idle connections to maintain |
| `connect_timeout_seconds` | `30` | Timeout for acquiring a connection |
| `idle_timeout_seconds` | `600` | Time before idle connections are closed |

```rust
use suprnova::database::DatabaseConfig;

let config = DatabaseConfig::builder()
    .url("postgres://localhost/myapp")
    .max_connections(20)      // For high-traffic applications
    .min_connections(5)       // Keep 5 connections warm
    .connect_timeout_seconds(10)
    .idle_timeout_seconds(300)
    .build();
```

## Database Initialization

The database is automatically initialized when your suprnova application starts. The `DB::init()` method is called during bootstrap:

```rust
// src/main.rs (handled automatically by suprnova)
use suprnova::database::{DB, DatabaseConfig};

#[tokio::main]
async fn main() {
    // Database initialization happens automatically
    // when you use suprnova::new()

    suprnova::new()
        .serve()
        .await;
}
```

### Manual Initialization

If you need to initialize the database manually:

```rust
use suprnova::database::{DB, DatabaseConfig};

async fn setup_database() {
    let config = DatabaseConfig::from_env();
    DB::init(config).await;
}
```

## Accessing the Connection

Use the `DB` facade to access the database connection anywhere in your application:

```rust
use suprnova::database::DB;
use sea_orm::EntityTrait;

async fn example() {
    // Get the database connection
    let db = DB::connection();

    // Use with SeaORM queries
    let users = users::Entity::find()
        .all(db)
        .await
        .unwrap();
}
```

## Multiple Databases

For applications requiring multiple database connections, you can manage connections manually:

```rust
use sea_orm::{Database, DatabaseConnection};

struct AppState {
    primary_db: DatabaseConnection,
    analytics_db: DatabaseConnection,
}

async fn setup_databases() -> AppState {
    let primary = Database::connect("postgres://localhost/primary")
        .await
        .unwrap();

    let analytics = Database::connect("postgres://localhost/analytics")
        .await
        .unwrap();

    AppState {
        primary_db: primary,
        analytics_db: analytics,
    }
}
```

## Environment-Specific Configuration

Use different configurations for development, testing, and production:

```env
# .env.development
DATABASE_URL=sqlite:./dev.db

# .env.test
DATABASE_URL=sqlite::memory:

# .env.production
DATABASE_URL=postgres://user:pass@prod-server:5432/myapp
```

## Troubleshooting

### Connection Refused

If you see connection refused errors:

1. Verify the database server is running
2. Check the connection string format
3. Ensure network access (firewall, security groups)

```bash
# Test PostgreSQL connection
psql -h localhost -U user -d myapp

# Test MySQL connection
mysql -h localhost -u user -p myapp
```

### Pool Exhausted

If connections are being exhausted:

1. Increase `max_connections`
2. Ensure connections are being released (avoid long-running transactions)
3. Check for connection leaks in your code

```rust
// Increase pool size
let config = DatabaseConfig::builder()
    .url("postgres://localhost/myapp")
    .max_connections(50)  // Increase from default 10
    .build();
```

### SQLite File Not Created

For SQLite, ensure the directory exists:

```rust
// The file will be created automatically, but the directory must exist
DATABASE_URL=sqlite:./data/database.db  // Ensure ./data/ exists
```
