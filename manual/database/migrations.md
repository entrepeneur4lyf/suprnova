---
title: 'Migrations'
description: 'Manage database schema changes with migrations'
icon: 'arrow-up'
---

Migrations allow you to evolve your database schema over time. suprnova uses SeaORM migrations with a Laravel-inspired workflow.

> **Info:**
>
> For CLI commands (`suprnova migratsuprnova `suprnova migrate:rollback`, etc.), see the [CLI Migrations Reference](/cli/migrations).


## Creating Migrations

Generate a new migration file:

```bash
suprnova make:migration create_users_table
```

This creates a timestamped migration file in the `migrations/` directory:

```
migrations/
├── mod.rs
└── m20240115_120000_create_users_table.rs
```

## Migration Structure

Every migration has two methods:

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    // Run the migration
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Create tables, add columns, etc.
    }

    // Reverse the migration
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Drop tables, remove columns, etc.
    }
}
```

## Schema Operations

### Creating Tables

```rust
use sea_orm_migration::prelude::*;

async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(Users::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(Users::Id)
                        .integer()
                        .not_null()
                        .auto_increment()
                        .primary_key(),
                )
                .col(ColumnDef::new(Users::Email).string().not_null().unique_key())
                .col(ColumnDef::new(Users::Name).string().not_null())
                .col(ColumnDef::new(Users::PasswordHash).string().not_null())
                .col(ColumnDef::new(Users::CreatedAt).timestamp().not_null())
                .col(ColumnDef::new(Users::UpdatedAt).timestamp().not_null())
                .to_owned(),
        )
        .await
}

// Define the table and column identifiers
#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    Email,
    Name,
    PasswordHash,
    CreatedAt,
    UpdatedAt,
}
```

### Dropping Tables

```rust
async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .drop_table(Table::drop().table(Users::Table).to_owned())
        .await
}
```

### Column Types

| Method | Database Type | Notes |
|--------|---------------|-------|
| `integer()` | INTEGER | 32-bit integer |
| `big_integer()` | BIGINT | 64-bit integer |
| `small_integer()` | SMALLINT | 16-bit integer |
| `float()` | FLOAT | Floating point |
| `double()` | DOUBLE | Double precision |
| `decimal()` | DECIMAL | Fixed-point |
| `string()` | VARCHAR(255) | Variable length string |
| `string_len(n)` | VARCHAR(n) | Custom length string |
| `text()` | TEXT | Long text |
| `boolean()` | BOOLEAN | True/false |
| `timestamp()` | TIMESTAMP | Date and time |
| `date()` | DATE | Date only |
| `time()` | TIME | Time only |
| `blob()` | BLOB | Binary data |
| `json()` | JSON | JSON data |
| `uuid()` | UUID | UUID type |

### Column Modifiers

```rust
ColumnDef::new(Column::Name)
    .string()
    .not_null()              // NOT NULL constraint
    .null()                  // Allows NULL (default)
    .default("value")        // Default value
    .unique_key()            // UNIQUE constraint
    .primary_key()           // PRIMARY KEY
    .auto_increment()        // AUTO_INCREMENT
```

## Adding Columns

```rust
async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(Users::Table)
                .add_column(
                    ColumnDef::new(Users::PhoneNumber)
                        .string()
                        .null()
                )
                .to_owned(),
        )
        .await
}

async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(Users::Table)
                .drop_column(Users::PhoneNumber)
                .to_owned(),
        )
        .await
}
```

## Modifying Columns

```rust
async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(Users::Table)
                .modify_column(
                    ColumnDef::new(Users::Name)
                        .string_len(500)  // Change VARCHAR(255) to VARCHAR(500)
                        .not_null()
                )
                .to_owned(),
        )
        .await
}
```

## Renaming Columns

```rust
async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(Users::Table)
                .rename_column(Users::Name, Users::FullName)
                .to_owned(),
        )
        .await
}
```

## Indexes

### Creating Indexes

```rust
async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .create_index(
            Index::create()
                .name("idx_users_email")
                .table(Users::Table)
                .col(Users::Email)
                .unique()  // Optional: make it unique
                .to_owned(),
        )
        .await
}
```

### Composite Indexes

```rust
manager
    .create_index(
        Index::create()
            .name("idx_posts_user_created")
            .table(Posts::Table)
            .col(Posts::UserId)
            .col(Posts::CreatedAt)
            .to_owned(),
    )
    .await
```

### Dropping Indexes

```rust
async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .drop_index(Index::drop().name("idx_users_email").to_owned())
        .await
}
```

## Foreign Keys

### Adding Foreign Keys

```rust
async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(Posts::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(Posts::Id)
                        .integer()
                        .not_null()
                        .auto_increment()
                        .primary_key(),
                )
                .col(ColumnDef::new(Posts::UserId).integer().not_null())
                .col(ColumnDef::new(Posts::Title).string().not_null())
                .col(ColumnDef::new(Posts::Content).text().not_null())
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_posts_user")
                        .from(Posts::Table, Posts::UserId)
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade)
                        .on_update(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await
}
```

### Foreign Key Actions

| Action | Description |
|--------|-------------|
| `Cascade` | Delete/update child rows automatically |
| `SetNull` | Set foreign key to NULL |
| `SetDefault` | Set foreign key to default value |
| `Restrict` | Prevent delete/update if referenced |
| `NoAction` | Similar to Restrict |

## Migration Workflow

### 1. Create Migration

```bash
suprnova make:migration create_posts_table
```

### 2. Edit the Migration

```rust
// migrations/m20240115_130000_create_posts_table.rs
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Posts::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Posts::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Posts::Title).string().not_null())
                    .col(ColumnDef::new(Posts::Body).text().not_null())
                    .col(ColumnDef::new(Posts::Published).boolean().not_null().default(false))
                    .col(ColumnDef::new(Posts::CreatedAt).timestamp().not_null())
                    .col(ColumnDef::new(Posts::UpdatedAt).timestamp().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Posts::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Posts {
    Table,
    Id,
    Title,
    Body,
    Published,
    CreatedAt,
    UpdatedAt,
}
```

### 3. Run the Migration

```bash
suprnova migrate
```

### 4. Sync Entities

```bash
suprnova db:sync
```

This generates the corresponding entity file in `src/models/`.

## Best Practices

### Always Write Down Migrations

Always implement `down()` to allow rollbacks:

```rust
// Good: Reversible migration
async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager.create_table(/* ... */).await
}

async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager.drop_table(/* ... */).await
}
```

### Use Descriptive Names

```bash
# Good: Describes the change
suprnova make:migration add_email_verified_to_users
suprnova make:migration create_order_items_table
suprnova make:migration add_index_to_posts_slug

# Bad: Vague names
suprnova make:migration update_users
suprnova make:migration change_table
```

### One Change Per Migration

Keep migrations focused on a single change:

```bash
# Good: Separate migrations
suprnova make:migration create_categories_table
suprnova make:migration add_category_id_to_posts

# Avoid: Multiple unrelated changes in one migration
```

### Test Migrations Both Ways

Before committing, verify both directions work:

```bash
suprnova migrate           # Apply
suprnova migrate:rollback  # Rollback
suprnova migrate           # Apply again
```

## CLI Commands Reference

| Command | Description |
|---------|-------------|
| `suprnova make:migration <name>` | Create a new migration |
| `suprnova migrate` | Run all pending migrations |
| `suprnova migrate:status` | Show migration status |
| `suprnova migrate:rollback` | Rollback last migration |
| `suprnova migrate:rollback --step 3` | Rollback last 3 migrations |
| `suprnova migrate:fresh` | Drop all tables and re-run |
| `suprnova db:sync` | Sync schema to entities |

See the [CLI Migrations Reference](/cli/migrations) for detailed command documentation.
