# Migrations

Migrations describe how your schema evolves — each file is a small Rust struct with `up()` and `down()` methods that the framework runs in timestamp order. Use them whenever you change tables, columns, indexes, or foreign keys; that change moves from your laptop to staging to production by running the same migrate command in each place.

Suprnova's migrations are SeaORM migrations underneath. The CLI generates them, the `Migrator` aggregates them, and `Application::migrations::<Migrator>()` plugs them into your app's boot. For the full per-command reference (flags, output samples, exit codes) see [CLI Migrations Reference](cli-migrations.md); this chapter covers what to put *inside* the files.

## Creating migrations

Generate a new migration file:

```bash
suprnova make:migration create_users_table
```

The generator writes a timestamped file under `src/migrations/` (creating the
directory the first time) and registers it in the `Migrator`:

```
src/migrations/
├── mod.rs                              ← the Migrator (CLI-managed)
└── m20240115_120000_create_users_table.rs
```

The filename is `m{YYYYMMDD}_{HHMMSS}_<name>.rs`; ordering is by filename, so
the timestamp prefix is what enforces a deterministic apply order.

### What the generator emits

`make:migration create_users_table` produces this skeleton:

```rust
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
                    .col(
                        ColumnDef::new(Users::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Users::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
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

The generator infers the table name from the migration name
(`create_X_table` → `X`, `add_Y_to_X` → `X`, `drop_X_table` → `X`). Anything
else becomes the literal name.

### The Migrator

`src/migrations/mod.rs` collects every migration into a single `Migrator`
that `MigratorTrait` walks. The CLI maintains this file when you
`make:migration`, so you rarely touch it by hand:

```rust
pub use sea_orm_migration::prelude::*;

mod m20240115_120000_create_users_table;
mod m20240115_130000_create_posts_table;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20240115_120000_create_users_table::Migration),
            Box::new(m20240115_130000_create_posts_table::Migration),
        ]
    }
}
```

Wire the migrator into your app's `main.rs` so `serve`, `migrate`,
`migrate:status`, `migrate:rollback`, and `migrate:fresh` all see the same
list:

```rust
use suprnova::Application;

#[tokio::main]
async fn main() {
    Application::new()
        .config(my_app::config::register)
        .bootstrap(my_app::bootstrap::bootstrap)
        .routes(my_app::routes::register)
        .migrations::<my_app::migrations::Migrator>()
        .run()
        .await
}
```

The scaffolder writes this for you on `suprnova new`.

### Why Suprnova diverges

Most of the framework deliberately hides SeaORM — you write `#[suprnova::model]`
and `User::query().db_where(...)`, not `Entity::find().filter(...)`. Migrations
are the one place we leave `sea_orm_migration::prelude::*` visible. Two reasons.

First, the schema-builder DSL is genuinely good and re-aliasing every name in
it (`Table`, `ColumnDef`, `Index`, `ForeignKey`, `Expr`, `ForeignKeyAction`,
`DeriveIden`, ...) would buy a longer import line and nothing else. Second,
migration files are pure Rust — your CI compiler verifies them — and that
catches more typos than any DSL re-aliasing would. We treat migrations like
schema-as-code, and the canonical SeaORM names *are* the schema vocabulary.

If you ever do need a SeaORM type the framework hasn't re-exported, the
escape hatch is `use suprnova::sea_orm;`. You almost never need it.

## Migration structure

Every migration has two methods:

```rust
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    // Apply the change
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> { /* ... */ }

    // Reverse the change
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> { /* ... */ }
}
```

Both arms return `Result<(), DbErr>` — bubble errors with `?` and the framework
turns a failed migration into a non-zero exit so deploy pipelines abort.

## Schema operations

### Creating tables

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

### Dropping tables

```rust
async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .drop_table(Table::drop().table(Users::Table).to_owned())
        .await
}
```

### Column types

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

### Column modifiers

```rust
ColumnDef::new(Column::Name)
    .string()
    .not_null()                                // NOT NULL constraint
    .null()                                    // Allows NULL (default)
    .default("value")                          // Default value
    .default(Expr::current_timestamp())        // Function default (e.g. NOW())
    .unique_key()                              // UNIQUE constraint
    .primary_key()                             // PRIMARY KEY
    .auto_increment()                          // AUTO_INCREMENT
```

For surrogate primary keys, prefer `big_integer().auto_increment().primary_key()`
on real tables — `INTEGER` (32-bit) is fine for tiny lookup tables but the
scaffolded `users`, `sessions`, and similar tables all use `BIGINT` because
a 4-byte counter is the kind of constraint you regret three years in.

## Adding columns

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

## Modifying columns

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

## Renaming columns

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

### Creating indexes

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

### Composite indexes

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

### Dropping indexes

```rust
async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .drop_index(Index::drop().name("idx_users_email").to_owned())
        .await
}
```

## Foreign keys

### Adding foreign keys

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

### Foreign key actions

| Action | Description |
|--------|-------------|
| `Cascade` | Delete/update child rows automatically |
| `SetNull` | Set foreign key to NULL |
| `SetDefault` | Set foreign key to default value |
| `Restrict` | Prevent delete/update if referenced |
| `NoAction` | Similar to Restrict |

## Migration workflow

A typical change goes through four steps:

```bash
# 1. Generate the file (creates src/migrations/m{ts}_create_posts_table.rs
#    and updates src/migrations/mod.rs).
suprnova make:migration create_posts_table

# 2. Edit src/migrations/m{ts}_create_posts_table.rs to define your schema.

# 3. Apply the migration.
suprnova migrate

# 4. Regenerate SeaORM entity files from the live schema so the models
#    compile against the new shape. `db:sync` also runs any pending
#    migrations first (use --skip-migrations to skip that step).
suprnova db:sync
```

`db:sync` writes auto-generated entity glue to `src/models/entities/<table>.rs`
and a user-editable stub to `src/models/<table>.rs`. Re-running it updates the
entity files; your user stubs are left alone unless you pass
`--regenerate-models` (which overwrites them — keep custom methods elsewhere
or version-control before you run it).

### Auto-migrate on serve

`suprnova serve` and `suprnova web:run` apply any pending migrations before
opening the HTTP socket. The default policy is **fail-closed**: if `up()`
errors, the process aborts non-zero before bind, so a broken migration can
never reach traffic.

Two escape hatches:

| Flag / env | Effect |
|---|---|
| `--no-migrate` (on `serve` / `web:run`) | Skip the auto-migrate step entirely. Useful when migrations run from a separate deploy step. |
| `SUPRNOVA_AUTO_MIGRATE_BEST_EFFORT=true` | Opt back into the legacy log-and-continue behaviour. The process keeps booting on a migration error. Not recommended in production. |

Background workers (`queue:work`, `workflow:work`, `schedule:run`) do *not*
auto-migrate — they assume schema is already in place when they boot, since
running migrations from N workers concurrently would race.

### Running migrations in tests

`TestDatabase::fresh::<Migrator>()` spins up an isolated in-memory SQLite
database, runs every migration, and binds the connection into the test
container so `DB::connection()` and `#[inject]` resolve to it:

```rust
use suprnova::testing::TestDatabase;
use crate::migrations::Migrator;

#[tokio::test]
async fn users_table_is_created() {
    let db = TestDatabase::fresh::<Migrator>().await.unwrap();
    // `db` is dropped at the end of the test, clearing the container.
}
```

See [Database Tests](database-testing.md) for the full pattern (factories,
parallel safety, picking a real driver instead of in-memory SQLite).

## Best practices

### Always write down migrations

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

### Use descriptive names

```bash
# Good: Describes the change
suprnova make:migration add_email_verified_to_users
suprnova make:migration create_order_items_table
suprnova make:migration add_index_to_posts_slug

# Bad: Vague names
suprnova make:migration update_users
suprnova make:migration change_table
```

### One change per migration

Keep migrations focused on a single change:

```bash
# Good: Separate migrations
suprnova make:migration create_categories_table
suprnova make:migration add_category_id_to_posts

# Avoid: Multiple unrelated changes in one migration
```

### Test migrations both ways

Before committing, verify both directions work:

```bash
suprnova migrate           # Apply
suprnova migrate:rollback  # Rollback
suprnova migrate           # Apply again
```

## CLI commands at a glance

| Command | Description |
|---------|-------------|
| `suprnova make:migration <name>` | Create a new migration |
| `suprnova migrate` | Run all pending migrations |
| `suprnova migrate:status` | Show migration status |
| `suprnova migrate:rollback` | Rollback the last migration |
| `suprnova migrate:rollback --step 3` | Rollback the last 3 migrations |
| `suprnova migrate:fresh` | Drop all tables and re-run every migration |
| `suprnova db:sync` | Run migrations and regenerate entity files |
| `suprnova db:sync --skip-migrations` | Regenerate entity files without applying migrations |
| `suprnova db:sync --regenerate-models` | Also overwrite user-editable model stubs |

See [CLI Migrations Reference](cli-migrations.md) for the full per-command
reference (flags, output samples, exit codes).

## Next

- [CLI Migrations Reference](cli-migrations.md) — flag-by-flag reference for `migrate*` and `db:sync`
- [Database](database.md) — connection configuration, transactions, read/write split
- [Eloquent](eloquent.md) — the model layer your migrations feed
- [Seeding](seeding.md) — populating tables once their schema exists
- [Database Tests](database-testing.md) — `TestDatabase::fresh::<Migrator>()` and parallel-safe patterns
