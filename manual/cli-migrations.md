# Database Migrations

suprnova provides a complete set of commands for managing database migrations, inspired by Laravel's migration system.

## make:migration

Generate a new database migration file.

```bash
suprnova make:migration <name>
```

### Examples

```bash
suprnova make:migration create_users_table
suprnova make:migration add_email_to_users
suprnova make:migration create_posts_table
```

### Generated File

```rust
// migrations/YYYYMMDDHHMMSS_create_users_table.rs
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
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
                    .col(ColumnDef::new(Users::CreatedAt).timestamp().not_null())
                    .col(ColumnDef::new(Users::UpdatedAt).timestamp().not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Users::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    CreatedAt,
    UpdatedAt,
}
```

---

## migrate

Run all pending migrations.

```bash
suprnova migrate
```

### Output

```
Running migrations...
Applying migration: 20240101120000_create_users_table
Applying migration: 20240101120001_create_posts_table
All migrations completed successfully!
```

---

## migrate:status

Show the status of all migrations.

```bash
suprnova migrate:status
```

### Output

```
Migration Status
----------------
[✓] 20240101120000_create_users_table (applied)
[✓] 20240101120001_create_posts_table (applied)
[ ] 20240101120002_add_email_to_users (pending)
```

---

## migrate:rollback

Rollback the last migration(s).

```bash
suprnova migrate:rollback [options]
```

### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--step <N>` | `1` | Number of migrations to rollback |

### Examples

```bash
# Rollback the last migration
suprnova migrate:rollback

# Rollback the last 3 migrations
suprnova migrate:rollback --step 3
```

### Output

```
Rolling back migrations...
Rolling back: 20240101120002_add_email_to_users
Rollback completed successfully!
```

---

## migrate:fresh

Drop all tables and re-run all migrations from scratch.

```bash
suprnova migrate:fresh
```

> **Warning:**
>
> This command will **delete all data** in your database. Use with caution, especially in production!


### Output

```
Dropping all tables...
Tables dropped.
Running migrations...
Applying migration: 20240101120000_create_users_table
Applying migration: 20240101120001_create_posts_table
Applying migration: 20240101120002_add_email_to_users
Fresh migration completed successfully!
```

### Use Cases

- Resetting development database
- Testing migrations from scratch
- Clearing test data

---

## db:sync

Sync database schema to entity files. This runs migrations and regenerates SeaORM entity files.

```bash
suprnova db:sync [options]
```

### Options

| Option | Description |
|--------|-------------|
| `--skip-migrations` | Skip running migrations before sync |
| `--regenerate-models` | Regenerate model files (overwrites existing custom models with new fluent API) |

### Examples

```bash
# Run migrations and sync entities
suprnova db:sync

# Only sync entities (skip migrations)
suprnova db:sync --skip-migrations

# Regenerate all model files with the latest fluent API
suprnova db:sync --regenerate-models
```

> **Warning:**
>
> The `--regenerate-models` flag will overwrite your custom model files in `src/models/`. Use with caution if you've added custom methods.


### What It Does

1. Runs all pending migrations (unless `--skip-migrations`)
2. Introspects the database schema
3. Generates/updates SeaORM entity files in `src/models/`

---

## Migration Workflow

A typical migration workflow:

```bash
# 1. Create a new migration
suprnova make:migration create_users_table

# 2. Edit the migration file to define your schema
# migrations/YYYYMMDDHHMMSS_create_users_table.rs

# 3. Run the migration
suprnova migrate

# 4. Sync entity files
suprnova db:sync

# 5. Use the generated model in your code
# src/models/users.rs is now available
```

---

## Writing Migrations

### Creating Tables

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
                .col(ColumnDef::new(Posts::Title).string().not_null())
                .col(ColumnDef::new(Posts::Content).text().not_null())
                .col(ColumnDef::new(Posts::UserId).integer().not_null())
                .foreign_key(
                    ForeignKey::create()
                        .from(Posts::Table, Posts::UserId)
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .col(ColumnDef::new(Posts::CreatedAt).timestamp().not_null())
                .col(ColumnDef::new(Posts::UpdatedAt).timestamp().not_null())
                .to_owned(),
        )
        .await
}
```

### Adding Columns

```rust
async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(Users::Table)
                .add_column(ColumnDef::new(Users::Email).string().not_null())
                .to_owned(),
        )
        .await
}

async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(Users::Table)
                .drop_column(Users::Email)
                .to_owned(),
        )
        .await
}
```

### Creating Indexes

```rust
async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .create_index(
            Index::create()
                .name("idx_users_email")
                .table(Users::Table)
                .col(Users::Email)
                .unique()
                .to_owned(),
        )
        .await
}
```

---

## Summary

| Command | Description |
|---------|-------------|
| `make:migration <name>` | Create a new migration file |
| `migrate` | Run all pending migrations |
| `migrate:status` | Show migration status |
| `migrate:rollback` | Rollback last migration(s) |
| `migrate:fresh` | Drop all tables and re-run migrations |
| `db:sync` | Sync schema to entity files |
